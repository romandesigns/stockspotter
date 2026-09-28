"""Tests for ops/observation/prereg_freeze.py (Step 4B-main.1 §13-14).

Synthetic identities only: no real preregistration or manifest is created.
"""
import importlib.util
import json
import pathlib
import sys

import pytest

ROOT = pathlib.Path(__file__).resolve().parents[2]
OBS = ROOT / "ops" / "observation"
sys.path.insert(0, str(OBS))
_spec = importlib.util.spec_from_file_location("prereg_freeze", OBS / "prereg_freeze.py")
pf = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(pf)
archive = pf.archive

TEMPLATE = OBS / "step4-preregistration-v1.fixture.json"


def _synthetic_tables(tmp_path):
    cond = tmp_path / "conditions.json"
    cond.write_text(json.dumps({"schema": "trade-condition-policy-v2", "version": "synthetic-test", "freezeStatus": "FINAL",
                                "tapes": {"C": {"include": ["@"], "exclude": ["I"], "censor": []}}}))
    status = tmp_path / "status.json"
    status.write_text(json.dumps({"schema": "trading-status-policy-v1", "version": "synthetic-test", "freezeStatus": "FINAL",
                                  "families": {"UTP": {"tapes": ["C"], "codes": {"H": {"class": "HALT"}, "T": {"class": "RESUME"}}}}}))
    return cond, status


PROTOCOL_BYTES = b"# synthetic protocol document for tests\n"


def _documents(tmp_path):
    """A synthetic protocol file (its SHA is the approved gate) and the real
    outcome-source contract document."""
    protocol = tmp_path / "protocol.md"
    protocol.write_bytes(PROTOCOL_BYTES)
    return protocol, OBS / "OUTCOME-SOURCE-CONTRACT.md"


def _approved(tmp_path, **over):
    cond, status = _synthetic_tables(tmp_path)
    a = {
        "captureMaxBytes": 16 * 1024 ** 3,
        "tradeConditionTableSha256": archive.prereg_sha(str(cond)),
        "statusPolicySha256": archive.prereg_sha(str(status)),
        "implementationSha": "a1b2c3d4e5" * 4,
        "outcomeSource": "synthetic-test-source",
        "freezeStatus": "FINAL",
        "overrides": {"censoring.limitedThresholdStatus": "FROZEN",
                      "gateSha256": archive.hashlib.sha256(PROTOCOL_BYTES).hexdigest()},
    }
    a.update(over)
    return a


def _template():
    return json.loads(TEMPLATE.read_text(encoding="utf-8"))


def test_the_fixture_template_cannot_be_frozen_as_is():
    with pytest.raises(pf.Refusal) as e:
        pf.build(_template(), {}, freeze=True)
    text = " ".join(e.value.problems)
    for key in ["captureMaxBytes", "tradeConditionTableSha256", "statusPolicySha256", "implementationSha", "outcomeSource"]:
        assert f"unresolved: {key}" in text, key
    assert "freezeStatus" in text


@pytest.mark.parametrize("override,needle", [
    ({"implementationSha": "0" * 40}, "placeholder"),
    ({"tradeConditionTableSha256": "4811c24baee8b97cdbafb4b5baf971c0db410760d2171fffc7458fc9b62a428b"}, "fixture identity"),
    ({"statusPolicySha256": "d7977ab9939b548cd3b4dbec90b36089662c727d8eb1308205cba766442b8067"}, "fixture identity"),
    ({"statusPolicySha256": "ABC"}, "hex64"),
    ({"outcomeSource": "PENDING: later"}, "unresolved marker"),
    ({"captureMaxBytes": 8 * 1024 ** 3}, "8 GiB placeholder"),
    ({"overrides": {}}, "PROPOSED"),  # censoring.limitedThresholdStatus left PROPOSED
    ({"freezeStatus": "NOT-FINAL-NOT-FROZEN"}, "requires FINAL"),
    ({"overrides": {"censoring.limitedThresholdStatus": "FROZEN", "no.such.field": 1}}, "no such field"),
])
def test_every_unresolved_placeholder_is_refused(tmp_path, override, needle):
    with pytest.raises(pf.Refusal) as e:
        pf.build(_template(), _approved(tmp_path, **override), freeze=True)
    assert any(needle in p for p in e.value.problems), e.value.problems


def test_a_deliberately_adopted_8_gib_budget_is_accepted(tmp_path):
    doc = pf.build(_template(), _approved(tmp_path, captureMaxBytes=8 * 1024 ** 3, captureMaxBytesConfirmed=True), freeze=True)
    assert doc["capture"]["maxBytes"] == 8 * 1024 ** 3


def test_generation_is_deterministic_final_and_never_overwrites(tmp_path):
    approved = tmp_path / "approved.json"
    approved.write_text(json.dumps(_approved(tmp_path)))
    a, b = tmp_path / "a.json", tmp_path / "b.json"
    sha_a = pf.generate(str(TEMPLATE), str(approved), str(a), freeze=True)
    sha_b = pf.generate(str(TEMPLATE), str(approved), str(b), freeze=True)
    assert sha_a == sha_b
    doc = json.loads(a.read_text(encoding="utf-8"))
    assert doc["freezeStatus"] == "FINAL" and "_fixture" not in doc
    assert doc["implementationSha"] == "a1b2c3d4e5" * 4
    with pytest.raises(FileExistsError):
        pf.generate(str(TEMPLATE), str(approved), str(a), freeze=True)


def test_the_freeze_manifest_rederives_and_binds_every_identity(tmp_path):
    approved = tmp_path / "approved.json"
    approved.write_text(json.dumps(_approved(tmp_path)))
    pre = tmp_path / "prereg.json"
    pre_sha = pf.generate(str(TEMPLATE), str(approved), str(pre), freeze=True)
    cond, status = tmp_path / "conditions.json", tmp_path / "status.json"
    out = tmp_path / "manifest.json"
    protocol, contract = _documents(tmp_path)
    pf.manifest(str(pre), str(cond), str(status), str(protocol), str(contract), str(out))
    m = json.loads(out.read_text())
    assert m["schema"] == pf.MANIFEST_SCHEMA
    assert m["preregistrationSha256"] == pre_sha
    assert m["tradeConditionTableSha256"] == archive.prereg_sha(str(cond))
    assert m["statusPolicySha256"] == archive.prereg_sha(str(status))
    assert m["implementationSha"] == "a1b2c3d4e5" * 4
    assert m["protocolSha256"] == archive.hashlib.sha256(protocol.read_bytes()).hexdigest() == json.loads(pre.read_text())["gateSha256"]
    assert m["outcomeContractDocumentSha256"] == archive.hashlib.sha256(contract.read_bytes()).hexdigest()
    assert m["outcomeFetchContract"] == "alpaca-v2-stocks-trades-v1"
    assert m["certificateSemantics"] == "pass-fail-indeterminate-v1"
    assert m["campaignRules"] == {"maxDesignatedSessions": 20, "qualifyingSessionsTarget": 10, "minDiscriminatingWindowsPerSession": 20}


def test_the_manifest_refuses_a_mismatched_table_or_an_unfrozen_preregistration(tmp_path):
    approved = tmp_path / "approved.json"
    approved.write_text(json.dumps(_approved(tmp_path)))
    pre = tmp_path / "prereg.json"
    pf.generate(str(TEMPLATE), str(approved), str(pre), freeze=True)
    cond, status = tmp_path / "conditions.json", tmp_path / "status.json"
    other = tmp_path / "other.json"
    other.write_text(json.dumps({"schema": "trade-condition-policy-v2", "version": "other", "tapes": {}}))
    with pytest.raises(pf.Refusal) as e:
        pf.manifest(str(pre), str(other), str(status), *map(str, _documents(tmp_path)), str(tmp_path / "m1.json"))
    assert any("condition table" in p for p in e.value.problems)
    with pytest.raises(pf.Refusal) as e:
        pf.manifest(str(TEMPLATE), str(cond), str(status), *map(str, _documents(tmp_path)), str(tmp_path / "m2.json"))
    assert any("not FINAL" in p for p in e.value.problems)


def test_the_manifest_refuses_tables_that_are_not_themselves_frozen(tmp_path):
    cond, status = _synthetic_tables(tmp_path)
    proposed = json.loads(cond.read_text())
    proposed["freezeStatus"] = "PROPOSED-NOT-FROZEN"
    cond.write_text(json.dumps(proposed))
    approved = _approved(tmp_path)  # rewrites the tables as FINAL
    cond.write_text(json.dumps(proposed))  # ...then the condition table reverts to PROPOSED
    approved["tradeConditionTableSha256"] = archive.prereg_sha(str(cond))
    (tmp_path / "approved.json").write_text(json.dumps(approved))
    pre = tmp_path / "prereg.json"
    pf.generate(str(TEMPLATE), str(tmp_path / "approved.json"), str(pre), freeze=True)
    with pytest.raises(pf.Refusal) as e:
        pf.manifest(str(pre), str(cond), str(status), *map(str, _documents(tmp_path)), str(tmp_path / "m.json"))
    assert any("condition table freezeStatus" in p for p in e.value.problems), e.value.problems
    # The proposed real tables are, correctly, not freezable as they stand.
    with pytest.raises(pf.Refusal) as e:
        pf.manifest(str(pre), str(OBS / "trade-conditions-v2.proposed.json"), str(OBS / "trading-status-policy-v1.proposed.json"), *map(str, _documents(tmp_path)), str(tmp_path / "m2.json"))
    assert any("not FINAL" in p for p in e.value.problems)


def test_the_manifest_hashes_the_protocol_file_and_refuses_one_that_is_not_the_gate(tmp_path):
    approved = tmp_path / "approved.json"
    approved.write_text(json.dumps(_approved(tmp_path)))
    pre = tmp_path / "prereg.json"
    pf.generate(str(TEMPLATE), str(approved), str(pre), freeze=True)
    cond, status = tmp_path / "conditions.json", tmp_path / "status.json"
    wrong = tmp_path / "CONTRACT.md"
    wrong.write_bytes(b"an L1 design document, not the frozen protocol\n")
    with pytest.raises(pf.Refusal) as e:
        pf.manifest(str(pre), str(cond), str(status), str(wrong), str(OBS / "OUTCOME-SOURCE-CONTRACT.md"), str(tmp_path / "m.json"))
    assert any("gateSha256" in p for p in e.value.problems)


def test_the_real_gate_document_hash_is_the_fixture_gate():
    """The protocol's identity is the Step-3 verdict's raw-byte SHA-256, which
    the fixture already carries as gateSha256 (evidence: research archive)."""
    fixture = json.loads(TEMPLATE.read_text(encoding="utf-8"))
    assert fixture["gateSha256"] == "46dfd17c727a03423d16174fe844b6f9f01b91488c9631348ea3331fe4531518"


def test_finalize_is_deterministic_and_refuses_open_markers():
    status = json.loads((OBS / "trading-status-policy-v1.proposed.json").read_text(encoding="utf-8"))
    a = pf.finalize(status, "final-v1")
    b = pf.finalize(status, "final-v1")
    assert a == b and a["freezeStatus"] == "FINAL" and a["version"] == "final-v1"
    assert pf.final_sha(str(OBS / "trading-status-policy-v1.proposed.json"), "final-v1") == \
        archive.hashlib.sha256(archive.canonical_bytes(a)).hexdigest()
    with pytest.raises(pf.Refusal):
        pf.finalize(status, "proposed-2")
    reopened = json.loads(json.dumps(status))
    reopened["families"]["CTA"]["codes"]["F"]["why"] += " DECISION ITEM."
    with pytest.raises(pf.Refusal):
        pf.finalize(reopened, "final-v1")
    # The condition table stays unfinalizable until the RTH check resolves it.
    cond = json.loads((OBS / "trade-conditions-v2.proposed.json").read_text(encoding="utf-8"))
    with pytest.raises(pf.Refusal):
        pf.finalize(cond, "final-v1")

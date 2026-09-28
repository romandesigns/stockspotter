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
    cond.write_text(json.dumps({"schema": "trade-condition-policy-v2", "version": "synthetic-test",
                                "tapes": {"C": {"include": ["@"], "exclude": ["I"], "censor": []}}}))
    status = tmp_path / "status.json"
    status.write_text(json.dumps({"schema": "trading-status-policy-v1", "version": "synthetic-test",
                                  "families": {"UTP": {"tapes": ["C"], "codes": {"H": {"class": "HALT"}, "T": {"class": "RESUME"}}}}}))
    return cond, status


def _approved(tmp_path, **over):
    cond, status = _synthetic_tables(tmp_path)
    a = {
        "captureMaxBytes": 16 * 1024 ** 3,
        "tradeConditionTableSha256": archive.prereg_sha(str(cond)),
        "statusPolicySha256": archive.prereg_sha(str(status)),
        "implementationSha": "a1b2c3d4e5" * 4,
        "outcomeSource": "synthetic-test-source",
        "freezeStatus": "FINAL",
        "overrides": {"censoring.limitedThresholdStatus": "FROZEN"},
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
    pf.manifest(str(pre), str(cond), str(status), "c" * 64, str(out))
    m = json.loads(out.read_text())
    assert m["schema"] == pf.MANIFEST_SCHEMA
    assert m["preregistrationSha256"] == pre_sha
    assert m["tradeConditionTableSha256"] == archive.prereg_sha(str(cond))
    assert m["statusPolicySha256"] == archive.prereg_sha(str(status))
    assert m["implementationSha"] == "a1b2c3d4e5" * 4
    assert m["protocolSha256"] == "c" * 64
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
        pf.manifest(str(pre), str(other), str(status), "c" * 64, str(tmp_path / "m1.json"))
    assert any("condition table" in p for p in e.value.problems)
    with pytest.raises(pf.Refusal) as e:
        pf.manifest(str(TEMPLATE), str(cond), str(status), "c" * 64, str(tmp_path / "m2.json"))
    assert any("not FINAL" in p for p in e.value.problems)

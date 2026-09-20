import copy
import importlib.util
import pathlib
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("observer", pathlib.Path(__file__).with_name("observe-session.py"))
observer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(observer)


def fixture():
    writer = dict(degraded=False, dropped=0, writeErrors=0, lossSpans=0, attempted=1, written=1,
                  queueCapacity=16384, queueCapacityBytes=64 * 1024**2)
    opening = "2026-09-21T08:00:00.123456789Z"
    common = dict(symbol="TEST", sessionDate="2026-09-21", openedAt=opening,
                  opportunityId="TEST:2026-09-21:28800123")
    rows = {
        "opportunity-outcomes": dict(common, anchorAt="2026-09-21T08:00:01Z",
            provenance=dict(measurementVersion="opportunity-outcome-v1", opportunitySchema=2,
                            configFingerprint=observer.FINGERPRINT),
            returns=[dict(horizonSecs=n) for n in observer.HORIZONS]),
        "opportunity-intelligence": dict(common, timestamp="2026-09-21T08:00:01Z", schemaVersion=2,
            versions=dict(opportunitySchema=2), observedHigh=11, observedLow=9, maxMovePct=10,
            minMovePct=-10, openingPrice=10),
        "episodes": dict(schemaVersion=2, id=dict(symbol="TEST"), openedAt=opening,
            openedBy="FastFunnel", episodeUid="epu1|TEST|" + opening + "|FastFunnel"),
    }
    report = dict(commit=observer.BASE, oiConfigFingerprint=observer.FINGERPRINT,
        opportunityEngine=dict(capacityEvictions=0, cohortTruncations=0),
        opportunityOutcomeEngine=dict(capacity=297000, capacityEvictions=0, anchorsCreated=1, anchorsSettled=1, outstanding=0))
    report.update({name: copy.deepcopy(writer) for name in ("measurement", "opportunityIntelligence", "opportunityOutcomes", "discovery")})
    return dict(date="2026-09-21", head=observer.BASE, deployed=observer.BASE, dirty="", configHash="a", qualificationHash="b",
        protected={f"/stockspotter-vps-{name}-1": dict(running=True, id=name, started="2026-09-19T19:16:55Z") for name in observer.SERVICES},
        mounts=[dict(Source="/opt/apps/stockspotter/data", Destination="/app/data", RW=True)], freeBytes=69 * 1024**3,
        health=dict(report=report, anyKnownLoss=False, retention=dict(retentionPending=False), measurementPending=dict(capacityEvictions=0)),
        samples={name: dict(rows=[row], malformed=0, partialTail=False) for name, row in rows.items()})


class ObserverTests(unittest.TestCase):
    def test_valid_first_traffic_sample_and_settlement_are_distinct_from_full_verdict(self):
        data = fixture()
        result = observer.assess(data, copy.deepcopy(data))
        self.assertEqual(result["status"], "PASS_SAMPLED_SMOKE")
        self.assertTrue(result["outcomeSettlementObserved"])
        self.assertIn("NOT_EVALUATED", result["fullSessionVerdict"])

    def test_weekend_and_missing_baseline_never_pass(self):
        data = fixture()
        data["health"]["report"]["opportunityOutcomeEngine"]["anchorsCreated"] = 0
        data["samples"] = {}
        self.assertEqual(observer.assess(data)["status"], "PENDING")

    def test_restarts_changed_fingerprint_or_loss_fail(self):
        for mutate in (
            lambda d: d["protected"]["/stockspotter-vps-ws-1"].update(id="restarted"),
            lambda d: d["health"]["report"].update(oiConfigFingerprint="different"),
            lambda d: d["health"]["report"]["opportunityOutcomes"].update(dropped=1),
            lambda d: d["health"]["report"]["opportunityOutcomeEngine"].update(capacityEvictions=1),
        ):
            original = fixture()
            changed = copy.deepcopy(original)
            mutate(changed)
            self.assertEqual(observer.assess(changed, original)["status"], "FAIL")

    def test_wrong_id_horizon_uid_or_stale_row_fails(self):
        for name, key, value in (
            ("opportunity-outcomes", "opportunityId", "TEST:2026-09-21:1"),
            ("opportunity-outcomes", "returns", []),
            ("episodes", "episodeUid", "epu1|invalid"),
            ("opportunity-intelligence", "timestamp", "2026-09-18T08:00:01Z"),
        ):
            original = fixture()
            changed = copy.deepcopy(original)
            changed["samples"][name]["rows"][0][key] = value
            self.assertEqual(observer.assess(changed, original)["status"], "FAIL")

    def test_partial_appending_line_is_pending_not_corruption(self):
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "capture.ndjson"
            path.write_bytes(b'{"ok":1}\n{"incomplete":')
            sample = observer.tail_sample(path)
            self.assertEqual(sample["rows"], [{"ok": 1}])
            self.assertTrue(sample["partialTail"])
            self.assertEqual(sample["malformed"], 0)

    def test_offline_reconciliation_detects_missing_and_duplicate_anchors(self):
        spec = importlib.util.spec_from_file_location("reconcile", pathlib.Path(__file__).with_name("reconcile-outcomes.py"))
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        import json
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            sample = fixture()["samples"]
            for stem, item in sample.items():
                row = item["rows"][0]
                if stem != "episodes":
                    row["windowId"] = "window-1"
                if stem == "opportunity-intelligence":
                    row["versions"]["configFingerprint"] = observer.FINGERPRINT
                if stem == "opportunity-outcomes":
                    row["fullyObserved"] = True
                (root / f"{stem}-2026-09-21.ndjson").write_text(json.dumps(row) + "\n")
            self.assertEqual(module.reconcile(root, "2026-09-21")["status"], "PASS_FILE_RECONCILIATION")
            outcomes = root / "opportunity-outcomes-2026-09-21.ndjson"
            original = outcomes.read_text()
            outcomes.write_text(original * 2)
            self.assertIn("duplicate_opportunity-outcomes", module.reconcile(root, "2026-09-21")["errors"])
            outcomes.write_text("")
            self.assertEqual(module.reconcile(root, "2026-09-21")["errors"]["missing_outcomes"], 1)


if __name__ == "__main__":
    unittest.main()

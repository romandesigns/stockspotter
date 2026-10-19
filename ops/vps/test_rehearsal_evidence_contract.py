"""A rejected candidate that never started is not a proven rollback rehearsal."""
from datetime import datetime, timezone
from pathlib import Path
from types import SimpleNamespace
import tempfile
import unittest
from unittest.mock import Mock

import deployment_host as adapter


class RehearsalEvidenceContract(unittest.TestCase):
    def test_failure_before_candidate_proof_cannot_mint_rehearsal_receipt(self):
        with tempfile.TemporaryDirectory() as directory:
            store = adapter.core.Store(Path(directory))
            baseline = {'source':'a'*40,'checkout_source':'a'*40,'branch':'release/baseline', 'images':{role:'sha256:'+'a'*64 for role in adapter.ROLES},'env_hash':'c'*64}
            candidate = dict(baseline, source='b'*40, checkout_source='b'*40, branch='master', images={role:'sha256:'+'b'*64 for role in adapter.ROLES})
            store.write('committed.json', {'version':2,'state':baseline})
            controller = object.__new__(adapter.Controller)
            controller.store = store
            controller.host = SimpleNamespace(policy=None, _require_legacy_timer_disarmed=Mock(return_value={}), snapshot=Mock(return_value=baseline), evidence={}, clock=lambda:datetime.now(timezone.utc), boot_id=lambda:'fixture-boot')
            controller._policy = Mock(return_value=SimpleNamespace(digest=lambda:'d'*64))
            controller._prepared = Mock(return_value={'prepared':{'state':candidate}})
            controller._transact = Mock(side_effect=adapter.Rejected('candidate checkout refused before recreation'))
            controller._receipt = Mock(return_value='fixture-receipt')
            controller._print = Mock()
            with self.assertRaises(adapter.Rejected):
                controller.rehearse(True)
            self.assertIsNone(store.read(adapter.REHEARSAL_NAME))
            controller._receipt.assert_not_called()


if __name__ == '__main__':
    unittest.main()

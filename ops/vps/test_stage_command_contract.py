"""The emitted stage controller plan must use the controller's real CLI."""
import os
import shlex
import unittest
from pathlib import PurePosixPath

import deployment_host
import stage_instance


class StageCommandContract(unittest.TestCase):
    def test_complete_emitted_commands_are_accepted_by_actual_parser(self):
        owner = PurePosixPath('/home/stage/private/stockspotter-stage-command-contract')
        commands = stage_instance.controller_commands(owner.name, owner)
        parser = deployment_host.build_parser()
        for command in commands:
            # Adoption deliberately asks for reviewed role decisions; its
            # placeholder is not an executable invocation yet.
            if '...' in command:
                continue
            with self.subTest(command=command):
                try:
                    arguments = parser.parse_args(shlex.split(command)[1:])
                except SystemExit as error:
                    self.fail(f'emitted stage command is not runnable: {command}: {error}')
                self.assertEqual(arguments.project, owner.name)
                self.assertEqual(arguments.stage_owner_root.as_posix(), str(owner))


@unittest.skipUnless(os.name == "posix", "Stage bind paths require Linux; run this real-Git case on the VPS fixture")
class ReleaseBaselineBranchContract(unittest.TestCase):
    def test_release_head_clone_has_the_existing_master_needed_by_promotion(self):
        import subprocess
        import test_stage_instance as fixtures
        if not fixtures.GIT or not fixtures.OPENSSL:
            self.skipTest('Git and OpenSSL are required')
        fixture = fixtures.PrepareTests(methodName='runTest')
        self.addCleanup(fixture.doCleanups)
        fixture.setUp()
        fixture.git('switch', '-c', 'release/baseline', fixture.baseline, cwd=fixture.source)
        manifest = fixture.prepare(checkout=fixture.baseline, branch='release/baseline')
        repo = fixture.owner / stage_instance.REPO_DIRECTORY
        result = subprocess.run([fixtures.GIT, 'rev-parse', '--verify', 'refs/heads/master'], cwd=str(repo), text=True, capture_output=True, timeout=30)
        self.assertEqual(result.returncode, 0, 'release-baseline clone must retain a local master: Host.checkout refuses to create one during promotion')
        self.assertEqual(result.stdout.strip(), fixture.candidate)
        self.assertEqual(manifest['clone']['head'], fixture.baseline)


class StageEndpointCredentialContract(unittest.TestCase):
    def test_preserved_exact_paper_url_is_metadata_on_an_isolated_network(self):
        project = 'stockspotter-stage-endpoint-contract'
        document = {'name': project, 'services': {
            'qualify': {'environment': ['ALPACA_TRADING_BASE=https://paper-api.alpaca.markets', 'ALPACA_API_KEY=staging-not-a-real-credential-key']},
            'broker': {'networks': {'default': {'aliases': ['paper-api.alpaca.markets']}}}},
            'networks': {'default': {'internal': True, 'name': project+'_default'}}}
        self.assertEqual(deployment_host.stage_recipe_failures(document, project), [], 'Linux driver probe failed before adoption: the exact public endpoint is not a credential; actual keys must still carry the synthetic marker')
        document['services']['qualify']['environment'][0] = 'ALPACA_TRADING_BASE=https://user:password@paper-api.alpaca.markets'
        self.assertTrue(deployment_host.stage_recipe_failures(document, project), 'An arbitrary URL cannot receive the exact-endpoint exception')


if __name__ == '__main__':
    unittest.main()

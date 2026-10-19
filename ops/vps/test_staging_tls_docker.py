"""Fixture lifecycle tests; no Docker or production dependency."""
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import staging_tls_docker as fixture


class FixtureLifecycleTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix='tls-docker-')
        self.addCleanup(self.directory.cleanup)
        self.owner = Path(self.directory.name)
        self.certificates = self.owner / 'certificates'
        self.certificates.mkdir(mode=0o700)
        for name in ('certificate.pem', 'key.pem'):
            (self.certificates / name).write_text('synthetic fixture bytes')

    def test_generated_key_and_certificate_are_removed_idempotently(self):
        fixture.remove_certificates(self.certificates)
        self.assertFalse(self.certificates.exists())
        fixture.remove_certificates(self.certificates)

    def test_unknown_contents_are_never_recursively_removed(self):
        unexpected = self.certificates / 'operator-notes.txt'
        unexpected.write_text('retain this file')
        with self.assertRaises(OSError): fixture.remove_certificates(self.certificates)
        self.assertEqual(unexpected.read_text(), 'retain this file')
        self.assertFalse((self.certificates / 'key.pem').exists())

    def test_an_unrelated_directory_is_not_cleaned(self):
        with self.assertRaises(RuntimeError): fixture.remove_certificates(self.owner)
        self.assertTrue((self.certificates / 'key.pem').exists())

    def test_substituted_symlink_paths_are_refused_before_unlink(self):
        with patch.object(Path, 'is_symlink', return_value=True):
            with self.assertRaises(RuntimeError): fixture.remove_certificates(self.certificates)
        self.assertTrue((self.certificates / 'key.pem').exists())

    def test_optimized_python_is_refused_before_any_fixture_effect(self):
        result = subprocess.run([sys.executable, '-O', str(Path(fixture.__file__)),
                                 '--evidence-root', str(self.owner / 'does-not-exist')],
                                capture_output=True, text=True, timeout=15)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('refuse python -O', result.stderr)


if __name__ == '__main__': unittest.main()

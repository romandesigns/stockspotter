"""Real verified HTTPS through a scoped synthetic resolver; no live broker."""
import contextlib
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import socket
import ssl
import tempfile
import threading
import unittest
from unittest.mock import patch
from http.server import ThreadingHTTPServer
from urllib.error import HTTPError

import staging_broker
import staging_tls

OPENSSL = shutil.which("openssl")
if not OPENSSL and os.name == "nt":
    candidate = Path(r"C:\Program Files\Git\usr\bin\openssl.exe")
    if candidate.is_file(): OPENSSL = str(candidate)

@unittest.skipUnless(OPENSSL, "OpenSSL CLI is required for the ephemeral fixture")
class VerifiedPreflightTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="stockspotter-tls-fixture-")
        self.root = Path(self.directory.name).resolve()
        assert self.root.is_relative_to(Path(tempfile.gettempdir()).resolve())
        self.addCleanup(self.directory.cleanup)
        self.fixture = staging_tls.provision(self.root / "tls", OPENSSL)
        self.preflight_path = Path(__file__).resolve().parents[1] / "release_preflight.py"
        if not self.preflight_path.is_file():
            # Immutable controller installations keep the unchanged preflight
            # beside their modules rather than in the repository layout.
            self.preflight_path = Path(__file__).with_name("release_preflight.py")
        self.preflight_digest = hashlib.sha256(self.preflight_path.read_bytes()).hexdigest()
        spec = importlib.util.spec_from_file_location("unchanged_release_preflight", self.preflight_path)
        self.preflight = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.preflight)
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), staging_broker.Handler)
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(self.fixture["certificate"], self.fixture["key"])
        self.server.socket = context.wrap_socket(self.server.socket, server_side=True)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.addCleanup(self.stop)
        self.resolver = socket.getaddrinfo

    def stop(self):
        self.server.shutdown(); self.server.server_close(); self.thread.join(timeout=5)

    def resolve(self, host, port, *args, **kwargs):
        if host == "paper-api.alpaca.markets":
            return self.resolver("127.0.0.1", self.server.server_port, *args, **kwargs)
        raise AssertionError("fixture attempted an unapproved destination")

    def run_preflight(self, **faults):
        environment = {"ALPACA_TRADING_BASE": staging_tls.PAPER_URL,
                       "ALPACA_API_KEY": "staging-not-a-real-credential-key",
                       "ALPACA_API_SECRET": "staging-not-a-real-credential-secret",
                       "STOCKSPOTTER_API_TOKEN": "staging-not-a-real-credential-token".ljust(48, "x"),
                       "SSL_CERT_FILE": self.fixture["certificate"],
                       "HTTP_PROXY": "", "HTTPS_PROXY": "", "ALL_PROXY": "", "NO_PROXY": "*"}
        environment.update(faults)
        output = io.StringIO()
        with patch.dict(os.environ, environment, clear=True), patch("socket.getaddrinfo", self.resolve), patch("urllib.request._opener", None), contextlib.redirect_stdout(output):
            self.preflight.main()
        self.assertEqual(hashlib.sha256(self.preflight_path.read_bytes()).hexdigest(), self.preflight_digest)
        return json.loads(output.getvalue())

    def test_exact_preserved_https_gate_can_prove_synthetic_zero(self):
        report = self.run_preflight()
        self.assertTrue(report["paper_endpoint_configured"])
        self.assertTrue(report["api_token_present_and_long_enough"])
        self.assertTrue(report["account_active"])
        self.assertIs(report["trading_blocked"], False)
        self.assertEqual((report["positions"], report["open_orders"]), (0, 0))

    def test_positions_orders_blocking_and_failure_are_not_masked(self):
        for name, field in [("STAGING_BROKER_POSITIONS", "positions"), ("STAGING_BROKER_OPEN_ORDERS", "open_orders")]:
            with self.subTest(name=name): self.assertEqual(self.run_preflight(**{name: "1"})[field], 1)
        self.assertIs(self.run_preflight(STAGING_BROKER_BLOCKED="1")["trading_blocked"], True)
        with self.assertRaises(HTTPError) as caught:
            self.run_preflight(STAGING_BROKER_UNREACHABLE="1")
        caught.exception.close()

    def test_an_untrusted_fixture_is_refused(self):
        # Preserve normal certificate verification. Without the ephemeral
        # trust file this self-signed server must fail, not look like zero.
        with self.assertRaises(Exception): self.run_preflight(SSL_CERT_FILE=str(self.root / "missing-ca.pem"))

    def test_certificate_directory_cannot_be_overwritten(self):
        with self.assertRaises(ValueError): staging_tls.provision(self.root / "tls", OPENSSL)

if __name__ == "__main__": unittest.main()

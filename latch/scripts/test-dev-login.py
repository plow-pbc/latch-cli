"""`dev-login.py` against a stand-in latch and Linq twin: no credential from
the environment reaches latch, a state folder logged in to another Plow is
refused before latch runs, and one logged in to this Plow logs in again."""
import http.server
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import unittest

HELPER = Path(__file__).with_name("dev-login.py")
API = "http://127.0.0.1:18934"

# A latch that records its environment and logs in by text.
FAKE_LATCH = """#!/usr/bin/env python3
import json, os, sys
with open(os.environ["SEEN"], "w") as seen:
    json.dump(dict(os.environ), seen)
print(json.dumps({"type": "code", "code": "123456", "send_to": "+15550000000"}), flush=True)
print(json.dumps({"type": "verified", "peer": {"name": "dev"}}), flush=True)
"""


class Twin(http.server.BaseHTTPRequestHandler):
    texts = []

    def do_POST(self):
        body = self.rfile.read(int(self.headers["content-length"]))
        Twin.texts.append(json.loads(body))
        self.send_response(200)
        self.send_header("content-length", "0")
        self.end_headers()

    def log_message(self, *args):
        pass


class DevLoginTests(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        root = Path(self.dir.name)
        self.latch, self.state, self.seen = root / "latch", root / "state", root / "seen.json"
        self.latch.write_text(FAKE_LATCH)
        self.latch.chmod(0o755)
        self.state.mkdir()
        self.twin = http.server.HTTPServer(("127.0.0.1", 0), Twin)
        threading.Thread(target=self.twin.serve_forever, daemon=True).start()
        Twin.texts.clear()

    def tearDown(self):
        self.twin.shutdown()
        self.dir.cleanup()

    def run_helper(self):
        env = {**os.environ, "SEEN": str(self.seen), "PLOW_AGENT_TOKEN": "made-up-prod-token",
               "PLOW_API_BASE": "https://api.plow.co", "PLOW_API_URL": "https://api.plow.co",
               "PLOW_TOKEN_FILE": "/config/secrets/plow-api-token"}
        twin = f"http://127.0.0.1:{self.twin.server_port}"
        return subprocess.run([sys.executable, str(HELPER), str(self.latch), str(self.state), API, twin],
                              env=env, capture_output=True, text=True)

    def test_no_credential_from_the_environment_reaches_latch(self):
        done = self.run_helper()
        self.assertEqual(done.returncode, 0, done.stderr)
        seen = json.loads(self.seen.read_text())
        for name in ("PLOW_AGENT_TOKEN", "PLOW_API_BASE", "PLOW_API_URL"):
            self.assertNotIn(name, seen)
        self.assertFalse(Path(seen["PLOW_TOKEN_FILE"]).exists())
        self.assertEqual(Twin.texts[0]["text"], "Plow Activate: 123456")

    def test_a_state_folder_logged_in_to_another_plow_is_refused_before_latch_runs(self):
        for name, text in (("config.json", json.dumps({"api_url": "https://api.plow.co"})),
                           ("plow-url", "https://api.plow.co\n")):
            for other in self.state.iterdir():
                other.unlink()
            (self.state / name).write_text(text)
            done = self.run_helper()
            self.assertEqual(done.returncode, 2, name)
            self.assertIn("is logged in to https://api.plow.co", done.stderr)
            self.assertFalse(self.seen.exists(), name)
            self.assertEqual(Twin.texts, [])

    def test_a_state_folder_logged_in_to_this_plow_logs_in_again(self):
        (self.state / "config.json").write_text(json.dumps({"api_url": API + "/"}))
        (self.state / "plow-url").write_text(API + "\n")
        done = self.run_helper()
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertIn("logged in to", done.stdout)


if __name__ == "__main__":
    unittest.main()

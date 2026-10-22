"""`dev-login.py` against a stand-in latch and Plow stack: no credential from
the environment reaches latch, a state folder logged in to another Plow is
refused before latch runs, one logged in to this Plow logs in again, and a
stack that isn't up is named before latch runs. `dev_plow.py`: where the
stack's addresses come from."""
import http.server
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import threading
import unittest

HELPER = Path(__file__).with_name("dev-login.py")
sys.path.insert(0, str(HELPER.parent))
import dev_plow  # noqa: E402

# A latch that records its environment and logs in by text.
FAKE_LATCH = """#!/usr/bin/env python3
import json, os, sys
with open(os.environ["SEEN"], "w") as seen:
    json.dump(dict(os.environ), seen)
print(json.dumps({"type": "code", "code": "123456", "send_to": "+15550000000"}), flush=True)
print(json.dumps({"type": "verified", "peer": {"name": "dev"}}), flush=True)
"""


# The stack's API and its Linq twin, on one port: any GET answers, and a
# POST is a text.
class Twin(http.server.BaseHTTPRequestHandler):
    texts = []

    def do_GET(self):
        self.send_response(404)
        self.send_header("content-length", "0")
        self.end_headers()

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
        self.api = f"http://127.0.0.1:{self.twin.server_port}"
        Twin.texts.clear()

    def tearDown(self):
        self.twin.shutdown()
        self.dir.cleanup()

    def run_helper(self, **plow):
        env = {k: v for k, v in os.environ.items() if not k.startswith("LATCH_DEV_")}
        env.update({"SEEN": str(self.seen), "PLOW_AGENT_TOKEN": "made-up-prod-token",
                    "PLOW_API_BASE": "https://api.plow.co", "PLOW_API_URL": "https://api.plow.co",
                    "PLOW_TOKEN_FILE": "/config/secrets/plow-api-token",
                    "LATCH_DEV_PLOW_API": self.api, "LATCH_DEV_PLOW_TWIN": self.api, **plow})
        return subprocess.run([sys.executable, str(HELPER), str(self.latch), str(self.state)],
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
        (self.state / "config.json").write_text(json.dumps({"api_url": self.api + "/"}))
        (self.state / "plow-url").write_text(self.api + "\n")
        done = self.run_helper()
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertIn("logged in to", done.stdout)

    def test_a_stack_that_is_not_up_is_named_before_latch_runs(self):
        with socket.socket() as closed:
            closed.bind(("127.0.0.1", 0))
            down = f"http://127.0.0.1:{closed.getsockname()[1]}"
        done = self.run_helper(LATCH_DEV_PLOW_API=down)
        self.assertEqual(done.returncode, 2)
        self.assertIn(f"API at {down} (LATCH_DEV_PLOW_API) isn't reachable", done.stderr)
        for name in ("LATCH_DEV_PLOW_CHECKOUT", "LATCH_DEV_PLOW_TWIN", ".env.local"):
            self.assertIn(name, done.stderr)
        self.assertFalse(self.seen.exists())


class MachineNameTests(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.root = Path(self.dir.name)

    def tearDown(self):
        self.dir.cleanup()

    def name(self, *args):
        """The --name latch is run with, for a state folder at `args`."""
        state = self.root.joinpath(*args)
        state.mkdir(parents=True)
        latch = self.root / "latch"
        latch.write_text("#!/bin/sh\necho \"$@\" > \"$SEEN\"\n")
        latch.chmod(0o755)
        seen = self.root / "seen"
        with http.server.HTTPServer(("127.0.0.1", 0), Twin) as stack:
            threading.Thread(target=stack.serve_forever, daemon=True).start()
            url = f"http://127.0.0.1:{stack.server_port}"
            subprocess.run([sys.executable, str(HELPER), str(latch), str(state), "--api", url, "--twin", url, *self.extra],
                           env={**os.environ, "SEEN": str(seen)}, capture_output=True, text=True)
            stack.shutdown()
        argv = seen.read_text().split()
        return argv[argv.index("--name") + 1]

    extra = []

    def test_each_checkout_s_machine_is_named_after_it(self):
        self.assertEqual(self.name("Latch CLI_wt.2", ".dev", "state"), "dev-latch-cli-wt-2")
        self.assertEqual(self.name("other", "state"), "dev-state")

    def test_name_still_names_it(self):
        self.extra = ["--name", "mine"]
        self.assertEqual(self.name("wt", ".dev", "state"), "mine")


class AddressTests(unittest.TestCase):
    def test_a_named_address_then_the_checkout_s_then_the_default(self):
        with tempfile.TemporaryDirectory() as checkout:
            Path(checkout, ".plow-dev-env").write_text(
                "# Generated by plow-dev-env\nPLOW_API_URL=http://127.0.0.1:18804\nPLOW_DTU_LINQ_PORT=18805\n")
            env = {dev_plow.CHECKOUT: checkout}
            self.assertEqual(dev_plow.address(dev_plow.API, env)[0], "http://127.0.0.1:18804")
            self.assertEqual(dev_plow.address(dev_plow.TWIN, env)[0], "http://127.0.0.1:18805")
            env[dev_plow.API] = "http://127.0.0.1:1"
            self.assertEqual(dev_plow.address(dev_plow.API, env), ("http://127.0.0.1:1", dev_plow.API))
        self.assertEqual(dev_plow.address(dev_plow.API, {}), ("http://127.0.0.1:18934", "the default"))
        self.assertEqual(dev_plow.address(dev_plow.TWIN, {})[0], "http://127.0.0.1:18935")

    def test_a_checkout_without_its_dev_env_says_so(self):
        with tempfile.TemporaryDirectory() as checkout:
            with self.assertRaisesRegex(dev_plow.Unset, "has no readable .plow-dev-env.*LATCH_DEV_PLOW_API"):
                dev_plow.address(dev_plow.API, {dev_plow.CHECKOUT: checkout})


if __name__ == "__main__":
    unittest.main()

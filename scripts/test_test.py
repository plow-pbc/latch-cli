"""scripts/test.py's own tests: which suite each test is in, what a plan
refuses, and that shards and suites lose no test."""
import importlib.util
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

spec = importlib.util.spec_from_file_location("runner", Path(__file__).with_name("test.py"))
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)


def plan(*argv, where="macos"):
    args = runner.argparse.Namespace(suites=list(argv), linux=False, shard=None, ci=False, dry_run=True)
    with mock.patch.object(runner, "where_we_are", return_value=where):
        return runner.plan(args)


class Suites(unittest.TestCase):
    def test_each_test_has_one_suite(self):
        cases = {
            ("latch", "vault::tests::a_value_is_sealed"): "core",
            ("latch", "sandbox::tests::a_profile"): "core",
            ("latch", "browser::core::tests::refs"): "core",
            ("latch", "browser::tests::cft::a_check"): "browser",
            ("latch", "browser::tests::fills::camoufox::mac::a_check"): "browser",
            ("latch", "serve::plugins_tests::pinned::google_is_enabled"): "browser",
            ("cli", "pinned::local_socket_browser_open"): "browser",
            ("cli", "scenario::the_owner_returns_items"): "scenario",
            ("cli", "lifecycle::a_detached_host_answers"): "lifecycle",
            ("cli", "messages::agent_help_says"): "messages",
            ("cli", "a_linux_latch_has_no_messages_or_contacts"): "core",
            ("setup", "setup_runs_every_step"): "core",
            ("worker", "an_exec_worker_writes_only_its_own_temp_folder"): "sandbox",
        }
        for (binary, name), suite in cases.items():
            self.assertEqual(runner.suite_of(binary, name), suite, name)

    def test_macos_runs_tagged_browser_tests_and_engines_linux_cannot(self):
        with mock.patch.object(runner, "pinned_engines", return_value={"cft": True, "camoufox": False}):
            self.assertTrue(runner.runs_on_macos("browser::tests::cft::mac::the_engine_runs"))
            self.assertTrue(runner.runs_on_macos("browser::tests::mac::camoufox_runs_with_page_windows"))
            self.assertTrue(runner.runs_on_macos("browser::tests::fills::camoufox::a_fill"))
            self.assertTrue(runner.runs_on_macos("serve::plugins_tests::pinned::google_is_enabled"))
            self.assertFalse(runner.runs_on_macos("browser::tests::fills::cft::a_fill"))
            self.assertFalse(runner.runs_on_macos("browser::tests::goals::fills_inside_plans"))
        with mock.patch.object(runner, "pinned_engines", return_value={"cft": True, "camoufox": True}):
            self.assertFalse(runner.runs_on_macos("browser::tests::fills::camoufox::a_fill"))
            self.assertTrue(runner.runs_on_macos("browser::tests::camoufox::mac::the_engine_runs"))

    def test_shards_split_every_test_once(self):
        names = [f"suite::test_{number}" for number in range(200)]
        for count in (1, 2, 3):
            buckets = [[n for n in names if runner.in_shard("latch", n, (i, count))] for i in range(1, count + 1)]
            self.assertCountEqual([n for bucket in buckets for n in bucket], names)


class Plans(unittest.TestCase):
    def test_no_suite_is_this_oss_gate(self):
        self.assertEqual(plan()[1], runner.GATE["macos"])
        self.assertEqual(plan(where="container")[1], runner.GATE["container"])

    def test_the_container_says_why_it_has_no_lifecycle(self):
        _, suites, _, not_here = plan(where="container")
        self.assertNotIn("lifecycle", suites)
        self.assertIn(("lifecycle", "not available in the container (runs in CI and on the Pi)"), not_here)
        with self.assertRaises(SystemExit) as refused:
            plan("lifecycle", where="container")
        self.assertIn("not available in the container", str(refused.exception))

    def test_a_suite_that_cannot_run_here_is_refused_not_skipped(self):
        for argv, where, says in ((["messages"], "container", "macOS only"),
                                  (["nonsense"], "macos", "no suite nonsense")):
            with self.assertRaises(SystemExit) as refused:
                plan(*argv, where=where)
            self.assertIn(says, str(refused.exception))

    def test_the_container_is_linux_arm64_on_any_host(self):
        for machine in ("x86_64", "arm64"):
            with mock.patch.object(runner.platform, "machine", return_value=machine):
                self.assertEqual(runner.pin_platform("container"), "linux_arm64")

    def test_a_named_suite_is_added_to_the_gate_once(self):
        self.assertEqual(plan("gate", "core")[1], runner.GATE["macos"])
        self.assertEqual(plan("scenario", "gate")[1], runner.GATE["macos"] + ["scenario"])


class Binaries(unittest.TestCase):
    def test_tests_run_by_name_and_ignored_ones_stay_ignored(self):
        with tempfile.TemporaryDirectory() as directory:
            crate = Path(directory) / "fixture"
            (crate / "src").mkdir(parents=True)
            (crate / "Cargo.toml").write_text('[package]\nname = "fixture"\nversion = "0.1.0"\nedition = "2024"\n')
            (crate / "src/main.rs").write_text(
                "fn main() {}\n#[test] fn a() {}\n#[test] fn aa() {}\n"
                "#[test] #[ignore] fn ignored() { panic!(); }\nmod mac { #[test] fn b() {} }\n")
            subprocess.run(["cargo", "generate-lockfile", "-q"], cwd=crate, check=True)
            built = subprocess.run(["cargo", "test", "-q", "--no-run", "--message-format=json"], cwd=crate,
                                   check=True, capture_output=True, text=True)
            executable = next(line for line in built.stdout.splitlines() if '"executable":"' in line
                              and '"test":true' in line)
            executable = runner.json.loads(executable)["executable"]
            names = runner.inventory(executable)
            self.assertCountEqual(names, ["a", "aa", "ignored", "mac::b"])
            ran = subprocess.run([executable, "--exact", "a", "ignored"], capture_output=True, text=True)
            self.assertEqual(ran.returncode, 0, ran.stdout)
            self.assertIn("test a ... ok", ran.stdout)
            self.assertIn("test ignored ... ignored", ran.stdout)
            self.assertNotIn("test aa ", ran.stdout)


if __name__ == "__main__":
    unittest.main()

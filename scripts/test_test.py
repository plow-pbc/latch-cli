"""scripts/test.py's own tests: each Rust test is in exactly one suite, as
nextest selects them, and a plan refuses what cannot run where it was asked."""
import importlib.util
import json
from pathlib import Path
import subprocess
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


# A crate shaped like latch: its binary and the cli and worker test binaries,
# with a test in each place a suite takes from, and each test's suite.
UNIT = {
    "vault::tests::sealed": "core",
    "browser::client::tests::usage": "core",
    "browser::tests::cft::a_check": "browser",
    "browser::tests::cft::mac::a_mac_check": "browser",
    "browser::tests::camoufox::a_check": "browser",
    "browser::tests::fills::cft::a_fill": "browser",
    "browser::tests::mac::camoufox_only": "browser",
    "serve::plugins_tests::pinned::gog": "browser",
}
CLI = {
    "a_login": "core",
    "a_linux_latch_has_no_messages": "core",
    "pinned::a_browser_call": "browser",
    "scenario::a_return": "scenario",
    "lifecycle::a_managed_host": "lifecycle",
    "messages::a_search": "messages",
}
WORKER = {"a_worker_write": "sandbox"}
# On macOS, with Camoufox pinned for macOS only: the browser tests it runs.
ON_MACOS = {"browser::tests::cft::mac::a_mac_check", "browser::tests::camoufox::a_check",
            "browser::tests::mac::camoufox_only", "serve::plugins_tests::pinned::gog",
            "pinned::a_browser_call"}


def source(names):
    """`names` as tests, each in its modules."""
    tree = {}
    for name in names:
        *modules, test = name.split("::")
        node = tree
        for module in modules:
            node = node.setdefault(module, {})
        node[test] = None

    def emit(node):
        return " ".join(f"#[test] fn {key}() {{}}" if child is None else f"mod {key} {{ {emit(child)} }}"
                        for key, child in node.items())
    return emit(tree) + "\n"


class Suites(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.directory = tempfile.TemporaryDirectory()
        crate = Path(cls.directory.name) / "latch"
        (crate / "src").mkdir(parents=True)
        (crate / "tests").mkdir()
        (crate / "Cargo.toml").write_text('[package]\nname = "latch"\nversion = "0.1.0"\nedition = "2024"\n')
        (crate / "src/main.rs").write_text("fn main() {}\n" + source(UNIT))
        (crate / "tests/cli.rs").write_text(source(CLI))
        (crate / "tests/worker.rs").write_text(source(WORKER))
        subprocess.run(["cargo", "generate-lockfile", "-q"], cwd=crate, check=True)
        cls.crate = crate

    @classmethod
    def tearDownClass(cls):
        cls.directory.cleanup()

    def selected(self, suite, where):
        listed = subprocess.run(["cargo", "nextest", "list", "--message-format", "json", "-E",
                                 runner.filterset(suite, where)], cwd=self.crate, check=True,
                                capture_output=True, text=True)
        return {f"{binary['binary-name']}:{name}"
                for binary in json.loads(listed.stdout)["rust-suites"].values()
                for name, case in binary["testcases"].items()
                if case["filter-match"]["status"] == "matches"}

    def test_each_test_is_in_exactly_one_suite(self):
        expected = {}
        for binary, tests in (("latch", UNIT), ("cli", CLI), ("worker", WORKER)):
            for name, suite in tests.items():
                expected.setdefault(suite, set()).add(f"{binary}:{name}")
        for suite in ("core", "sandbox", "messages", "browser", "lifecycle", "scenario"):
            self.assertEqual(self.selected(suite, "linux"), expected.get(suite, set()), suite)

    def test_macos_runs_tagged_browser_tests_and_engines_linux_cannot(self):
        with mock.patch.object(runner, "pinned_engines", return_value={"cft": True, "camoufox": False}):
            got = {name.split(":", 1)[1] for name in self.selected("browser", "macos")}
        self.assertEqual(got, ON_MACOS)
        with mock.patch.object(runner, "pinned_engines", return_value={"cft": True, "camoufox": True}):
            got = {name.split(":", 1)[1] for name in self.selected("browser", "macos")}
        self.assertEqual(got, ON_MACOS - {"browser::tests::camoufox::a_check"})


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


if __name__ == "__main__":
    unittest.main()

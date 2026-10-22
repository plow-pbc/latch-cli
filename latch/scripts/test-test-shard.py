"""Check partition coverage and invocation against an actual libtest executable."""
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("shard", Path(__file__).with_name("test-shard.py"))
shard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(shard)


class PartitionTests(unittest.TestCase):
    def test_every_test_has_exactly_one_shard(self):
        names = [f"suite::test_{number}" for number in range(100)]
        for count in (1, 3, 4):
            buckets = [shard.partition("latch", names, index, count) for index in range(1, count + 1)]
            assigned = [name for bucket in buckets for name in bucket]
            self.assertCountEqual(assigned, names)
            self.assertEqual(len(set(assigned)), len(names))
            self.assertEqual(shard.partition("latch", names[::-1], 1, count)[::-1], buckets[0])

    def test_real_cargo_binaries_are_partitioned_without_losing_ignored_tests(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            crate = root / "fixture"
            (crate / "src").mkdir(parents=True)
            (crate / "tests").mkdir()
            (crate / "Cargo.toml").write_text('[package]\nname = "shard-fixture"\nversion = "0.1.0"\nedition = "2024"\n')
            (crate / "src/main.rs").write_text(
                "fn main() {}\n#[test] fn a() {}\n#[test] fn aa() {}\n#[test] #[ignore] fn ignored() { panic!(); }\n")
            (crate / "tests/integration.rs").write_text("#[test] fn integration() {}\n")
            subprocess.run(["cargo", "generate-lockfile"], cwd=crate, check=True)
            assignments = {}
            for index in range(1, 4):
                result = subprocess.run(["python3", str(Path(shard.__file__).resolve()), str(index), "3"],
                                        cwd=crate, capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
                report = json.loads((root / f".dev/ci/shard-{index}.json").read_text())
                for binary in report["binaries"]:
                    assigned = assignments.setdefault(binary["name"], [])
                    assigned.extend(binary["selected"])
                    self.assertEqual(binary["exit"], 0)
                    for name in binary["selected"]:
                        outcome = "ignored" if name == "ignored" else "ok"
                        self.assertIn(f"test {name} ... {outcome}", result.stdout)
            self.assertCountEqual(assignments["shard-fixture"], ["a", "aa", "ignored"])
            self.assertEqual(assignments["integration"], ["integration"])


if __name__ == "__main__":
    unittest.main()

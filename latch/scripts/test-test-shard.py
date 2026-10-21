"""Check partition coverage and invocation against an actual libtest executable."""
import importlib.util
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

    def test_exact_filters_run_only_selected_tests(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "tests.rs"
            source.write_text("#[test] fn a() {}\n#[test] fn aa() {}\n#[test] #[ignore] fn ignored() { panic!(); }\n")
            executable = root / "tests"
            subprocess.run(["rustc", "--test", str(source), "-o", str(executable)], check=True)
            names = shard.inventory(str(executable))
            self.assertCountEqual(names, ["a", "aa", "ignored"])
            result = subprocess.check_output([str(executable), "--exact", "a", "ignored"], text=True)
            self.assertIn("1 passed; 0 failed; 1 ignored", result)
            self.assertIn("1 filtered out", result)


if __name__ == "__main__":
    unittest.main()

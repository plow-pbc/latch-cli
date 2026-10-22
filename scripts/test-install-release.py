"""Exercise release selection, verification, and setup without network or user state."""
import hashlib
import io
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest

INSTALLER = Path(__file__).resolve().parents[1] / "install.sh"


class ReleaseInstall(unittest.TestCase):
    def run_install(self, system, machine, target, corrupt=False, relative_state=False,
                    relative_bin=False, release="v0.1.0"):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            caller = root / "caller"
            caller.mkdir()
            state_arg = "profiles/team state" if relative_state else str(root / "state")
            expected_state = caller / state_arg
            tools = root / "bin"
            tools.mkdir()
            archive = root / f"latch-v0.1.0-{target}.tar.gz"
            folder = f"latch-v0.1.0-{target}"
            with tarfile.open(archive, "w:gz") as tar:
                for name, body in {
                    "install-binary.sh": '''#!/bin/sh
printf '%s\\n' "$@" > "$RESULT"
mkdir -p "$2"
touch "$2/setup-marker"
if [ "$3" = --bin-dir ]; then
  mkdir -p "$4"
  touch "$4/bin-marker"
fi
''',
                    "latch": '#!/bin/sh\necho "latch 0.1.0 fixture"\n',
                }.items():
                    info = tarfile.TarInfo(f"{folder}/{name}")
                    data = body.encode()
                    info.size, info.mode = len(data), 0o755
                    tar.addfile(info, io.BytesIO(data))
            digest = "0" * 64 if corrupt else hashlib.sha256(archive.read_bytes()).hexdigest()
            (root / "SHA256SUMS").write_text(f"{digest}  {archive.name}\n")
            (tools / "gh").write_text('''#!/bin/sh
if [ "$1 $2" = "release view" ]; then echo v0.1.0; exit; fi
while [ "$#" -gt 0 ]; do
  if [ "$1" = --dir ]; then shift; destination=$1; fi
  shift
done
cp "$FIXTURE"/*.tar.gz "$FIXTURE/SHA256SUMS" "$destination/"
''')
            (tools / "uname").write_text(
                f'#!/bin/sh\ncase "$1" in -s) echo {system};; -m) echo {machine};; esac\n'
            )
            for tool in tools.iterdir():
                tool.chmod(0o755)
            result_file = root / "result"
            options = ["--state-dir", state_arg]
            if relative_bin:
                options += ["--bin-dir", "apps/team tools"]
            result = subprocess.run(
                ["sh", str(INSTALLER), *([release] if release else []), *options],
                cwd=caller,
                env={**os.environ, "PATH": f"{tools}:{os.environ['PATH']}",
                     "FIXTURE": str(root), "RESULT": str(result_file)},
                capture_output=True, text=True,
            )
            if corrupt:
                self.assertNotEqual(result.returncode, 0, result.stdout)
                self.assertFalse(result_file.exists(), "setup ran before checksum verification")
            else:
                self.assertEqual(result.returncode, 0, result.stderr)
                arguments = result_file.read_text().splitlines()
                self.assertEqual(arguments[:-1], options)
                self.assertEqual(Path(arguments[-1]).name, "latch")
                self.assertTrue((expected_state / "setup-marker").exists(),
                                "setup must use the caller's state directory, surviving cleanup")
                self.assertIn("latch 0.1.0 fixture", result.stdout)
                if relative_bin:
                    self.assertTrue((caller / "apps/team tools/bin-marker").exists())

    def test_all_targets_and_setup_arguments(self):
        for system, machine, target in [
            ("Darwin", "arm64", "aarch64-apple-darwin"),
            ("Darwin", "x86_64", "x86_64-apple-darwin"),
            ("Linux", "x86_64", "x86_64-unknown-linux-musl"),
            ("Linux", "aarch64", "aarch64-unknown-linux-musl"),
        ]:
            with self.subTest(target=target):
                self.run_install(system, machine, target)

    def test_relative_state_directory_survives_download_cleanup(self):
        self.run_install("Darwin", "arm64", "aarch64-apple-darwin", relative_state=True)

    def test_latest_installs_the_newest_published_release(self):
        self.run_install("Darwin", "arm64", "aarch64-apple-darwin", release="latest")

    def test_no_tag_installs_the_newest_published_release(self):
        self.run_install("Darwin", "arm64", "aarch64-apple-darwin", release=None)

    def test_relative_bin_directory_survives_download_cleanup(self):
        self.run_install("Darwin", "arm64", "aarch64-apple-darwin", relative_bin=True)

    def test_bad_checksum_never_runs_setup(self):
        self.run_install("Darwin", "arm64", "aarch64-apple-darwin", corrupt=True)


if __name__ == "__main__":
    unittest.main()

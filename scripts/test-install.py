"""Run the packaged installer with local, hash-verified plugin fixtures."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]


class BinaryInstall(unittest.TestCase):
    def install(self, custom=False, source="last", no_args=False):
        parent = ROOT / ".dev/tests"
        parent.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=parent) as directory:
            root = Path(directory)
            setup = root / "setup"
            (setup / "src/google").mkdir(parents=True)
            shutil.copyfile(ROOT / "latch/install.sh", setup / "install.sh")
            binary = setup / "latch"
            binary.write_text("#!/bin/sh\necho installed-fixture\n")
            binary.chmod(0o755)
            home = root / "home"
            state = home / ".latch" if no_args else root / "state"
            gog = state / "plugins/gog/1.0/gog"
            gog.parent.mkdir(parents=True)
            gog.write_bytes(b"local-gog-fixture")
            (setup / "src/google/pin.json").write_text(json.dumps({
                "version": "1.0", "sha256": hashlib.sha256(gog.read_bytes()).hexdigest(),
            }, indent=2))
            tools = root / "tools"
            tools.mkdir()
            uname = tools / "uname"
            uname.write_text('#!/bin/sh\ncase "$1" in -s) echo FixtureOS;; -m) echo x86_64;; esac\n')
            uname.chmod(0o755)
            options = [] if no_args else ["--state-dir", str(state)]
            destination = home / ".local/bin/latch"
            if custom:
                options += ["--bin-dir", "team tools/bin"]
                destination = root / "team tools/bin/latch"
            if no_args or source == "implicit":
                arguments = options
            elif source == "first":
                arguments = [str(binary), *options]
            elif source == "middle":
                arguments = [*options[:2], str(binary), *options[2:]]
            else:
                arguments = [*options, str(binary)]
            args = ["sh", str(setup / "install.sh"), *arguments]
            env = {**os.environ, "HOME": str(home), "PATH": f"{tools}:{os.environ['PATH']}"}
            result = subprocess.run(args, cwd=root, env=env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(destination.read_bytes(), binary.read_bytes())
            self.assertEqual(subprocess.check_output([str(destination)], text=True), "installed-fixture\n")
            if custom:
                self.assertFalse((home / ".local/bin/latch").exists())
            again = subprocess.run(args, cwd=root, env=env, capture_output=True, text=True)
            self.assertEqual(again.returncode, 0, again.stderr)
            self.assertIn("latch already installed", again.stdout)

    def test_default_bin_directory(self):
        self.install()

    def test_relative_bin_directory_with_spaces(self):
        self.install(custom=True)

    def test_no_arguments_installs_the_adjacent_binary(self):
        self.install(no_args=True)

    def test_options_without_a_binary_path(self):
        self.install(custom=True, source="implicit")

    def test_options_after_the_binary(self):
        self.install(custom=True, source="first")

    def test_options_on_both_sides_of_the_binary(self):
        self.install(custom=True, source="middle")

    def test_macos_release_installs_the_app_and_updates_it_in_place(self):
        parent = ROOT / ".dev/tests"
        parent.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=parent) as directory:
            root = Path(directory)
            setup = root / "setup"
            (setup / "src/google").mkdir(parents=True)
            (setup / "src/browser").mkdir()
            shutil.copyfile(ROOT / "latch/install.sh", setup / "install.sh")
            # A release's layout: the app, and `latch` beside install.sh leading into it.
            app = setup / "Plow Latch.app"
            (app / "Contents/MacOS").mkdir(parents=True)
            (app / "Contents/Info.plist").write_text("<plist>co.plow.latch.cli</plist>\n")
            inner = app / "Contents/MacOS/latch"
            (setup / "latch").symlink_to("Plow Latch.app/Contents/MacOS/latch")
            home = root / "home"
            state = root / "state"
            gog = state / "plugins/gog/1.0/gog"
            gog.parent.mkdir(parents=True)
            gog.write_bytes(b"local-gog-fixture")
            (setup / "src/google/pin.json").write_text(json.dumps({
                "version": "1.0", "sha256": hashlib.sha256(gog.read_bytes()).hexdigest(),
            }, indent=2))
            # Chrome for Testing already in place, so nothing is downloaded.
            cft_pin = '{\n  "version": "1.0"\n}\n'
            (setup / "src/browser/cft.json").write_text(cft_pin)
            (state / "plugins/cft/1.0").mkdir(parents=True)
            (state / "plugins/cft/1.0/manifest.json").write_text(cft_pin)
            tools = root / "tools"
            tools.mkdir()
            uname = tools / "uname"
            uname.write_text('#!/bin/sh\ncase "$1" in -s) echo Darwin;; -m) echo arm64;; esac\n')
            uname.chmod(0o755)
            env = {**os.environ, "HOME": str(home), "PATH": f"{tools}:{os.environ['PATH']}"}
            args = ["sh", str(setup / "install.sh"), "--state-dir", str(state), str(setup / "latch")]
            installed = home / "Applications/Plow Latch.app"
            link = home / ".local/bin/latch"

            for build in ["one", "two"]:
                inner.write_text(f"#!/bin/sh\necho app-{build}\n")
                inner.chmod(0o755)
                result = subprocess.run(args, cwd=root, env=env, capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn("installed Plow Latch.app", result.stdout)
                self.assertTrue(link.is_symlink())
                self.assertEqual(os.readlink(link), str(installed / "Contents/MacOS/latch"))
                self.assertEqual(subprocess.check_output([str(link)], text=True), f"app-{build}\n")
                self.assertEqual((installed / "Contents/Info.plist").read_text(),
                                 "<plist>co.plow.latch.cli</plist>\n")
            self.assertFalse((home / "Applications/Plow Latch.app.new").exists())
            again = subprocess.run(args, cwd=root, env=env, capture_output=True, text=True)
            self.assertEqual(again.returncode, 0, again.stderr)
            self.assertIn("Plow Latch.app already installed", again.stdout)
            self.assertIn("latch already linked", again.stdout)

            # An update whose final rename fails keeps the working app and link.
            failing_mv = tools / "mv"
            failing_mv.write_text('#!/bin/sh\ncase "$1" in *.app.new) exit 73;; esac\nexec /bin/mv "$@"\n')
            failing_mv.chmod(0o755)
            inner.write_text("#!/bin/sh\necho app-three\n")
            failed = subprocess.run(args, cwd=root, env=env, capture_output=True, text=True)
            self.assertNotEqual(failed.returncode, 0, failed.stdout)
            self.assertIn("still at", failed.stderr)
            self.assertEqual(subprocess.check_output([str(link)], text=True), "app-two\n")
            leftovers = sorted(path.name for path in (home / "Applications").iterdir())
            self.assertEqual(leftovers, ["Plow Latch.app"])

            # A run cut short with the old app moved aside: the next run puts
            # the new one in place and leaves nothing behind.
            failing_mv.unlink()
            installed.rename(home / "Applications/Plow Latch.app.old")
            resumed = subprocess.run(args, cwd=root, env=env, capture_output=True, text=True)
            self.assertEqual(resumed.returncode, 0, resumed.stderr)
            self.assertEqual(subprocess.check_output([str(link)], text=True), "app-three\n")
            leftovers = sorted(path.name for path in (home / "Applications").iterdir())
            self.assertEqual(leftovers, ["Plow Latch.app"])

    def test_help_needs_no_binary_or_setup(self):
        result = subprocess.run([str(ROOT / "latch/install.sh"), "--help"],
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        options = ["--bin-dir", "--state-dir"]
        if (ROOT / "latch/src/browser/camoufox.json").exists():
            options.append("--with-camoufox")
        for option in options:
            self.assertIn(option, result.stdout)


if __name__ == "__main__":
    unittest.main()

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

    def test_help_needs_no_binary_or_setup(self):
        result = subprocess.run([str(ROOT / "latch/install.sh"), "--help"],
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        options = ["--bin-dir", "--state-dir"]
        if (ROOT / "latch/src/browser/camoufox.json").exists():
            options.append("--with-camoufox")
        for option in options:
            self.assertIn(option, result.stdout)



class MessagesReaderInstall(unittest.TestCase):
    def test_a_mac_install_puts_the_reader_in_this_latchs_plugin_folder(self):
        parent = ROOT / ".dev/tests"
        parent.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=parent) as directory:
            root = Path(directory)
            setup = root / "setup"
            (setup / "src/google").mkdir(parents=True)
            shutil.copyfile(ROOT / "latch/install.sh", setup / "install.sh")
            binary = setup / "latch"
            binary.write_text('#!/bin/sh\necho "latch 9.9.9 (fixture)"\n')
            binary.chmod(0o755)
            (setup / "latch-messages").write_bytes(b"reader-fixture")
            state = root / "state"
            gog = state / "plugins/gog/1.0/gog"
            gog.parent.mkdir(parents=True)
            gog.write_bytes(b"local-gog-fixture")
            (setup / "src/google/pin.json").write_text(json.dumps({
                "version": "1.0", "sha256": hashlib.sha256(gog.read_bytes()).hexdigest(),
            }, indent=2))
            tools = root / "tools"
            tools.mkdir()
            uname = tools / "uname"
            uname.write_text('#!/bin/sh\ncase "$1" in -s) echo Darwin;; -m) echo arm64;; esac\n')
            uname.chmod(0o755)
            args = ["sh", str(setup / "install.sh"), "--state-dir", str(state), "--bin-dir", str(root / "bin")]
            env = {**os.environ, "HOME": str(root / "home"), "PATH": f"{tools}:{os.environ['PATH']}"}
            result = subprocess.run(args, cwd=root, env=env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            reader = state / "plugins/messages/9.9.9/latch-messages"
            self.assertEqual(reader.read_bytes(), b"reader-fixture")
            self.assertTrue(os.access(reader, os.X_OK))

if __name__ == "__main__":
    unittest.main()

"""Run the packaged installer with local, hash-verified plugin fixtures."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
import zipfile


ROOT = Path(__file__).resolve().parents[1]


def seed_gog(setup, state):
    """gog already in `state`, as `setup`'s pin names it: nothing to download."""
    gog = state / "plugins/gog/1.0/gog"
    gog.parent.mkdir(parents=True, exist_ok=True)
    gog.write_bytes(b"local-gog-fixture")
    pin = setup / "src/google/pin.json"
    pin.parent.mkdir(parents=True, exist_ok=True)
    pin.write_text(json.dumps({
        "version": "1.0", "sha256": hashlib.sha256(gog.read_bytes()).hexdigest()}, indent=2))


class BinaryInstall(unittest.TestCase):
    def install(self, custom=False, source="last", no_args=False):
        parent = ROOT / ".dev/tests"
        parent.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=parent) as directory:
            root = Path(directory)
            setup = root / "setup"
            (setup / "src/google").mkdir(parents=True)
            shutil.copyfile(ROOT / "latch/install-binary.sh", setup / "install-binary.sh")
            # On this fixture OS Chrome for Testing has no build: says so.
            shutil.copyfile(ROOT / "latch/browser-engines.sh", setup / "browser-engines.sh")
            binary = setup / "latch"
            binary.write_text("#!/bin/sh\necho installed-fixture\n")
            binary.chmod(0o755)
            home = root / "home"
            state = home / ".latch" if no_args else root / "state"
            seed_gog(setup, state)
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
            args = ["sh", str(setup / "install-binary.sh"), *arguments]
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
            shutil.copyfile(ROOT / "latch/install-binary.sh", setup / "install-binary.sh")
            shutil.copyfile(ROOT / "latch/browser-engines.sh", setup / "browser-engines.sh")
            # A release's layout: the app, and `latch` beside install-binary.sh leading into it.
            app = setup / "Plow Latch.app"
            (app / "Contents/MacOS").mkdir(parents=True)
            (app / "Contents/Info.plist").write_text("<plist>co.plow.latch.cli</plist>\n")
            inner = app / "Contents/MacOS/latch"
            (setup / "latch").symlink_to("Plow Latch.app/Contents/MacOS/latch")
            home = root / "home"
            state = root / "state"
            seed_gog(setup, state)
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
            args = ["sh", str(setup / "install-binary.sh"), "--state-dir", str(state), str(setup / "latch")]
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
        result = subprocess.run([str(ROOT / "latch/install-binary.sh"), "--help"],
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        for option in ["--bin-dir", "--state-dir", "--browser", "--no-browser"]:
            self.assertIn(option, result.stdout)


def tool(folder, name, body):
    path = folder / name
    path.write_text("#!/bin/sh\n" + body)
    path.chmod(0o755)


class BrowserEngines(unittest.TestCase):
    """install-binary.sh's engine choice, and browser-engines.sh installing Chrome for
    Testing from a local, hash-checked fixture."""

    def setUp(self):
        parent = ROOT / ".dev/tests"
        parent.mkdir(parents=True, exist_ok=True)
        self.directory = tempfile.TemporaryDirectory(dir=parent)
        self.root = Path(self.directory.name)
        self.tools = self.root / "tools"
        self.tools.mkdir()
        self.env = {**os.environ, "HOME": str(self.root / "home"),
                    "PATH": f"{self.tools}:{os.environ['PATH']}"}

    def tearDown(self):
        self.directory.cleanup()

    def engines_chosen(self, *options):
        """The engines install-binary.sh hands browser-engines.sh, or None if it runs none."""
        setup = self.root / "setup"
        shutil.rmtree(setup, ignore_errors=True)
        (setup / "src/google").mkdir(parents=True)
        shutil.copyfile(ROOT / "latch/install-binary.sh", setup / "install-binary.sh")
        tool(setup, "latch", "echo installed-fixture\n")
        state = self.root / "state"
        seed_gog(setup, state)
        chosen = self.root / "chosen"
        chosen.unlink(missing_ok=True)
        tool(setup, "browser-engines.sh", f'shift 2\necho "$@" > "{chosen}"\n')
        tool(self.tools, "uname", 'case "$1" in -s) echo FixtureOS;; -m) echo x86_64;; esac\n')
        result = subprocess.run(["sh", str(setup / "install-binary.sh"), "--state-dir", str(state),
                                 *options, str(setup / "latch")],
                                cwd=self.root, env=self.env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        return chosen.read_text().strip() if chosen.exists() else None

    def test_chrome_by_default_none_with_no_browser_or_the_ones_named(self):
        self.assertEqual(self.engines_chosen(), "chrome")
        self.assertIsNone(self.engines_chosen("--no-browser"))
        self.assertEqual(self.engines_chosen("--browser", "camoufox"), "camoufox")
        self.assertEqual(self.engines_chosen("--browser", "chrome", "--browser", "camoufox"),
                         "chrome camoufox")

    def test_no_browser_and_browser_together_or_an_unknown_engine_are_refused(self):
        for options, says in ((["--no-browser", "--browser", "chrome"], "both"),
                              (["--browser", "firefox"], "usage")):
            result = subprocess.run(["sh", str(ROOT / "latch/install-binary.sh"), *options],
                                    env=self.env, capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0, options)
            self.assertIn(says, result.stderr, options)

    def chrome_fixture(self, tools):
        """A browser-engines.sh beside a pin for a local CfT archive, on a
        Linux arm64 machine whose curl serves only that archive. Returns the
        script's folder, the archive, and the file listing each download."""
        setup = self.root / "engines"
        (setup / "src/browser").mkdir(parents=True)
        shutil.copyfile(ROOT / "latch/browser-engines.sh", setup / "browser-engines.sh")
        archive = self.root / "cft.zip"
        with zipfile.ZipFile(archive, "w") as zip_file:
            zip_file.writestr("chrome-linux-arm64/chrome", "#!/bin/sh\n")
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        # The pin's fixed shape, as browser-engines.sh reads it with sed.
        (setup / "src/browser/cft.json").write_text(f"""{{
  "version": "1.0.0",
  "builds": {{
    "linux_arm64": {{
      "url": "https://example.invalid/cft.zip",
      "zip_sha256": "{digest}",
      "folder": "chrome-linux-arm64"
    }}
  }}
}}
""")
        tool(tools, "uname", 'case "$1" in -s) echo Linux;; -m) echo aarch64;; esac\n')
        fetched = self.root / "fetched"
        # Serves the fixture for the pinned URL; anything else fails.
        tool(tools, "curl", f"""out=""; url=""
while [ $# -gt 0 ]; do case "$1" in -o) out=$2; shift 2;; --proto) shift 2;; -*) shift;; *) url=$1; shift;; esac; done
[ "$url" = https://example.invalid/cft.zip ] || exit 22
echo "$url" >> "{fetched}"
cp "{archive}" "$out"
""")
        return setup, archive, fetched

    def test_chrome_installs_the_pinned_build_once_and_checks_its_hash(self):
        setup, archive, fetched = self.chrome_fixture(self.tools)
        tool(self.tools, "Xvfb", "exit 0\n")
        state = self.root / "state"

        def run():
            return subprocess.run(
                ["sh", str(setup / "browser-engines.sh"), "--state-dir", str(state), "chrome"],
                env=self.env, capture_output=True, text=True)
        first = run()
        self.assertEqual(first.returncode, 0, first.stderr)
        installed = state / "plugins/cft/1.0.0"
        self.assertTrue((installed / "chrome-linux-arm64/chrome").is_file())
        self.assertEqual((installed / "manifest.json").read_bytes(),
                         (setup / "src/browser/cft.json").read_bytes())
        again = run()
        self.assertEqual(again.returncode, 0, again.stderr)
        self.assertIn("already installed", again.stdout)
        self.assertEqual(fetched.read_text().count("\n"), 1, "downloaded once")

        # A download whose hash isn't the pin's installs nothing.
        shutil.rmtree(state)
        archive.write_bytes(b"not the pinned zip")
        bad = run()
        self.assertNotEqual(bad.returncode, 0)
        self.assertIn("nothing installed", bad.stderr)
        self.assertFalse((state / "plugins/cft/1.0.0/manifest.json").exists())

    def test_chrome_set_up_after_no_browser_installs_unzip_and_xvfb_first(self):
        # A Pi with neither unzip nor Xvfb: only the tools named here are on
        # its PATH, and its package manager installs the two.
        tools = self.root / "pi"
        tools.mkdir()
        for name in ["sh", "dirname", "sed", "cksum", "mkdir", "mktemp", "cp", "mv", "rm",
                     "cut", "id", "cat", "chmod", "grep", "tr", "sha256sum", "shasum", "perl"]:
            found = shutil.which(name)
            if found:
                (tools / name).symlink_to(found)
        unzip = shutil.which("unzip")
        self.assertIsNotNone(unzip, "the test unpacks its fixture with the real unzip")
        log = self.root / "root-log"
        tool(tools, "bwrap", "exit 0\n")
        tool(tools, "sudo", f'echo "sudo $*" >> "{log}"\nexec "$@"\n')
        tool(tools, "apt-get", f"""PATH=/usr/bin:/bin
echo "apt-get $*" >> "{log}"
[ "$1" = install ] || exit 0
for package in "$@"; do case "$package" in
  unzip) ln -sf "{unzip}" "{tools}/unzip" ;;
  xvfb) printf '#!/bin/sh\\nexit 0\\n' > "{tools}/Xvfb"; chmod 755 "{tools}/Xvfb" ;;
esac; done
""")
        setup, _, fetched = self.chrome_fixture(tools)
        env = {**self.env, "PATH": str(tools)}
        state = self.root / "state"

        # The installer with --no-browser installs neither.
        (setup / "src/google").mkdir(parents=True)
        shutil.copyfile(ROOT / "latch/install-binary.sh", setup / "install-binary.sh")
        tool(setup, "latch", "echo installed-fixture\n")
        seed_gog(setup, state)
        installed = subprocess.run(
            ["sh", str(setup / "install-binary.sh"), "--state-dir", str(state), "--no-browser",
             str(setup / "latch")], env=env, capture_output=True, text=True)
        self.assertEqual(installed.returncode, 0, installed.stderr)
        self.assertFalse((tools / "unzip").exists() or (tools / "Xvfb").exists())
        self.assertFalse((state / "plugins/cft").exists())

        # Later, what `latch browser setup --engine chrome` runs.
        def setup_chrome():
            return subprocess.run(
                ["sh", str(setup / "browser-engines.sh"), "--state-dir", str(state), "chrome"],
                env=env, capture_output=True, text=True)
        later = setup_chrome()
        self.assertEqual(later.returncode, 0, later.stderr)
        self.assertIn("installing as root: unzip xvfb", later.stdout)
        self.assertIn("apt-get install -y -q unzip xvfb", log.read_text())
        self.assertIn("sudo sh -c", log.read_text())
        self.assertTrue((tools / "Xvfb").exists())
        self.assertTrue((state / "plugins/cft/1.0.0/manifest.json").is_file())
        self.assertEqual(fetched.read_text().count("\n"), 1)

        again = setup_chrome()
        self.assertEqual(again.returncode, 0, again.stderr)
        installs = [line for line in log.read_text().splitlines() if line.startswith("apt-get install")]
        self.assertEqual(len(installs), 1, "installed once")


if __name__ == "__main__":
    unittest.main()

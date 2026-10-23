"""Run the packaged installer with local, hash-verified plugin fixtures."""
import fcntl
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import tarfile
import termios
import threading
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


def fixture(root, state, binary_script, system="FixtureOS", machine="x86_64", macos="26.0"):
    """install-binary.sh, with the scripts it runs, beside a fake latch, with
    a local gog pinned in `state`, a `uname` that names `system` and
    `machine`, and a `sw_vers` that names `macos`. Returns the setup folder,
    the binary and the folder to put first on PATH."""
    setup = root / "setup"
    setup.mkdir()
    for name in ["install-binary.sh", "install-packages.sh", "browser-engines.sh"]:
        shutil.copyfile(ROOT / "latch" / name, setup / name)
    binary = setup / "latch"
    binary.write_text(binary_script)
    binary.chmod(0o755)
    seed_gog(setup, state)
    tools = root / "tools"
    tools.mkdir()
    uname = tools / "uname"
    uname.write_text(f'#!/bin/sh\ncase "$1" in -s) echo {system};; -m) echo {machine};; esac\n')
    uname.chmod(0o755)
    sw_vers = tools / "sw_vers"
    sw_vers.write_text(f'#!/bin/sh\necho {macos}\n')
    sw_vers.chmod(0o755)
    return setup, binary, tools


def drain(terminal):
    """Reads a terminal's main side until its other side is closed."""
    try:
        while os.read(terminal, 4096):
            pass
    except OSError:
        pass


def at_terminal(args, cwd, env):
    """Runs `args` at a terminal of its own, as a person's: what /dev/tty opens."""
    main, side = os.openpty()
    # Read what it shows, so it never waits on a full terminal.
    shown = threading.Thread(target=drain, args=(main,))
    shown.start()
    result = subprocess.run(
        args, cwd=cwd, env=env, stdin=subprocess.DEVNULL, stdout=side,
        stderr=side, start_new_session=True, timeout=60,
        preexec_fn=lambda: fcntl.ioctl(1, termios.TIOCSCTTY, 0))
    os.close(side)
    shown.join()
    os.close(main)
    return result


class BinaryInstall(unittest.TestCase):
    def install(self, custom=False, source="last", no_args=False):
        parent = ROOT / ".dev/tests"
        parent.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=parent) as directory:
            root = Path(directory)
            home = root / "home"
            state = home / ".latch" if no_args else root / "state"
            # On this fixture OS Chrome for Testing has no build: says so.
            setup, binary, tools = fixture(root, state, "#!/bin/sh\necho installed-fixture\n")
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

    def test_latch_state_dir_names_the_state_folder_without_the_option(self):
        parent = ROOT / ".dev/tests"
        parent.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=parent) as directory:
            root = Path(directory)
            state = root / "state"
            setup, binary, tools = fixture(root, state, "#!/bin/sh\n")
            env = {**os.environ, "HOME": str(root / "home"), "LATCH_STATE_DIR": str(state),
                   "PATH": f"{tools}:{os.environ['PATH']}"}
            result = subprocess.run(["sh", str(setup / "install-binary.sh"), "--no-browser", str(binary)],
                                    cwd=root, env=env, capture_output=True, text=True,
                                    stdin=subprocess.DEVNULL)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn(f"gog 1.0 already installed at {state}/plugins/gog/1.0/gog", result.stdout)

    def test_setup_runs_only_at_a_terminal_and_not_when_told_not_to(self):
        parent = ROOT / ".dev/tests"
        parent.mkdir(parents=True, exist_ok=True)
        # An upgrade (a login there already) never runs it, even at a terminal.
        for terminal, extra, upgrade, runs in [
                (False, [], False, False), (True, [], False, True),
                (True, ["--non-interactive"], False, False), (True, [], True, False)]:
            with tempfile.TemporaryDirectory(dir=parent) as directory:
                root = Path(directory)
                state = root / "state"
                called = root / "called"
                setup, binary, tools = fixture(root, state,
                                               f'#!/bin/sh\necho "$@" >> "{called}"\n')
                if upgrade:
                    (state / "config.json").write_text("{}")
                env = {**os.environ, "HOME": str(root / "home"),
                       "PATH": f"{tools}:{os.environ['PATH']}"}
                args = ["sh", str(setup / "install-binary.sh"), "--state-dir", str(state),
                        "--no-browser", *extra, str(binary)]
                if terminal:
                    result = at_terminal(args, root, env)
                else:
                    result = subprocess.run(args, cwd=root, env=env, capture_output=True,
                                            stdin=subprocess.DEVNULL)
                self.assertEqual(result.returncode, 0)
                ran = called.read_text() if called.exists() else ""
                self.assertEqual(ran, f"--state-dir {state} setup\n" if runs else "",
                                 (terminal, extra, upgrade))

    def test_a_first_mac_install_at_a_terminal_opens_the_app_from_macos_26(self):
        parent = ROOT / ".dev/tests"
        parent.mkdir(parents=True, exist_ok=True)
        # What a first install ends with: the app opened (its arguments), or
        # `latch setup` run (latch's), or neither. An explicit --state-dir,
        # here a relative one, reaches the app as an absolute path.
        for macos, given, extra, upgrade, opens, runs in [
                ("26.0", True, [], False, "--args --state-dir {state}", ""),
                ("27.1", False, [], False, "", ""),
                ("26.0", True, ["--non-interactive"], False, None, ""),
                ("26.0", True, [], True, None, ""),
                ("15.7.1", True, [], False, None, "--state-dir state setup\n")]:
            case = (macos, given, extra, upgrade)
            with tempfile.TemporaryDirectory(dir=parent) as directory:
                root = Path(directory)
                home = root / "home"
                state = root / "state" if given else home / ".latch"
                called, opened = root / "called", root / "opened"
                setup, _, tools = fixture(root, state, "", system="Darwin", machine="arm64",
                                          macos=macos)
                (setup / "latch").unlink()
                inner = setup / "Plow Latch.app/Contents/MacOS/latch"
                inner.parent.mkdir(parents=True)
                inner.write_text(f'#!/bin/sh\necho "$@" >> "{called}"\n')
                inner.chmod(0o755)
                (setup / "latch").symlink_to("Plow Latch.app/Contents/MacOS/latch")
                tool(tools, "open", f'echo "$@" >> "{opened}"\n')
                if upgrade:
                    (state / "config.json").write_text("{}")
                env = {**os.environ, "HOME": str(home), "PATH": f"{tools}:{os.environ['PATH']}"}
                options = ["--state-dir", "state"] if given else []
                result = at_terminal(["sh", str(setup / "install-binary.sh"), *options,
                                      "--no-browser", *extra, str(setup / "latch")], root, env)
                self.assertEqual(result.returncode, 0, case)
                app = home / "Applications/Plow Latch.app"
                expected = None if opens is None else f"{app} {opens.format(state=state)}".strip() + "\n"
                self.assertEqual(opened.read_text() if opened.exists() else None, expected, case)
                self.assertEqual(called.read_text() if called.exists() else "", runs, case)

    def test_a_mac_before_13_is_refused_and_nothing_installed(self):
        parent = ROOT / ".dev/tests"
        parent.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=parent) as directory:
            root = Path(directory)
            home = root / "home"
            state = root / "state"
            setup, binary, tools = fixture(root, state, "", system="Darwin", machine="x86_64",
                                           macos="12.7.6")
            env = {**os.environ, "HOME": str(home), "PATH": f"{tools}:{os.environ['PATH']}"}
            result = subprocess.run(["sh", str(setup / "install-binary.sh"), "--state-dir", str(state),
                                     "--no-browser", str(binary)],
                                    cwd=root, env=env, capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("latch needs macOS 13 or later, and this Mac runs macOS 12.7.6", result.stderr)
            self.assertFalse(home.exists())

    def test_macos_release_installs_the_app_and_updates_it_in_place(self):
        parent = ROOT / ".dev/tests"
        parent.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=parent) as directory:
            root = Path(directory)
            setup = root / "setup"
            (setup / "src/google").mkdir(parents=True)
            (setup / "src/browser").mkdir()
            shutil.copyfile(ROOT / "latch/install-binary.sh", setup / "install-binary.sh")
            shutil.copyfile(ROOT / "latch/install-packages.sh", setup / "install-packages.sh")
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
            tool(tools, "sw_vers", "echo 26.0\n")
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

            # A new window with the same latch is a new app, and installs.
            (app / "Contents/Frameworks").mkdir()
            (app / "Contents/Frameworks/libPlowLatchUI.dylib").write_text("window-two\n")
            window = subprocess.run(args, cwd=root, env=env, capture_output=True, text=True)
            self.assertEqual(window.returncode, 0, window.stderr)
            self.assertIn("installed Plow Latch.app", window.stdout)
            self.assertEqual((installed / "Contents/Frameworks/libPlowLatchUI.dylib").read_text(),
                             "window-two\n")

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

    def test_client_only_installs_latch_alone_and_asks_nothing(self):
        parent = ROOT / ".dev/tests"
        parent.mkdir(parents=True, exist_ok=True)
        # A fresh Linux machine and a fresh macOS 26 one, each at a terminal:
        # no gog pin, Messages reader, bubblewrap or engine script is there to
        # find, root is never asked for, no setup runs and no app opens.
        for system in ["Linux", "Darwin"]:
            with tempfile.TemporaryDirectory(dir=parent) as directory:
                root = Path(directory)
                home = root / "home"
                state = home / ".latch"
                called, asked = root / "called", root / "asked"
                setup = root / "setup"
                setup.mkdir()
                shutil.copyfile(ROOT / "latch/install-binary.sh", setup / "install-binary.sh")
                tools = root / "tools"
                tools.mkdir()
                link_basic_tools(tools)
                for name in ["readlink", "basename", "ln", "diff"]:
                    (tools / name).symlink_to(shutil.which(name))
                tool(tools, "uname", f'case "$1" in -s) echo {system};; -m) echo arm64;; esac\n')
                tool(tools, "sw_vers", "echo 26.0\n")
                for name in ["sudo", "apt-get", "open", "curl"]:
                    tool(tools, name, f'echo "{name} $*" >> "{asked}"\n')
                latch = f'#!/bin/sh\necho "$@" >> "{called}"\n'
                if system == "Darwin":
                    inner = setup / "Plow Latch.app/Contents/MacOS/latch"
                    inner.parent.mkdir(parents=True)
                    inner.write_text(latch)
                    inner.chmod(0o755)
                    (setup / "latch").symlink_to("Plow Latch.app/Contents/MacOS/latch")
                    (setup / "Plow Latch.app/Contents/MacOS/latch-messages").write_text("reader")
                    installed = home / "Applications/Plow Latch.app/Contents/MacOS/latch"
                else:
                    tool(setup, "latch", latch.removeprefix("#!/bin/sh\n"))
                    installed = home / ".local/bin/latch"
                env = {**os.environ, "HOME": str(home), "PATH": str(tools)}
                result = at_terminal(["sh", str(setup / "install-binary.sh"), "--client-only"],
                                     root, env)
                self.assertEqual(result.returncode, 0, system)
                self.assertEqual(installed.read_text(), latch, system)
                self.assertTrue((home / ".local/bin/latch").exists(), system)
                self.assertFalse(state.exists(), system)
                self.assertFalse(asked.exists(), asked.read_text() if asked.exists() else system)
                self.assertFalse(called.exists(), system)

                # What it says last is to log in, with nothing else to set up.
                said = subprocess.run(["sh", str(setup / "install-binary.sh"), "--client-only",
                                       "--state-dir", str(root / "other")],
                                      cwd=root, env=env, capture_output=True, text=True)
                self.assertEqual(said.returncode, 0, said.stderr)
                self.assertIn("next: run `latch login` to log in", said.stdout)
                self.assertNotIn("setup", said.stdout)

    def test_client_only_installs_no_browser_so_one_named_is_refused(self):
        result = subprocess.run(["sh", str(ROOT / "latch/install-binary.sh"), "--client-only",
                                 "--browser", "chrome", "/nonexistent/latch"],
                                capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("--client-only installs no browser", result.stderr)

    def test_help_needs_no_binary_or_setup(self):
        result = subprocess.run([str(ROOT / "latch/install-binary.sh"), "--help"],
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        for option in ["--bin-dir", "--state-dir", "--browser", "--no-browser", "--client-only"]:
            self.assertIn(option, result.stdout)


def fake_latch(version, log, managed=True, stop_ok=True, start_ok=True, running=True,
               attached=False, other_version=False, stop=None):
    """A latch that says `version`, reports a host (managed or not, attached
    or not, another version's or not) on `status --local`, and writes each
    stop and start it does to `log`. `stop`, if given, is its whole stop."""
    blocked = '{"code":"other_version","message":"a host from another latch version"}'
    serve = '{"this_machine":{"hosting":{"running":%s,"ready":%s,"managed":%s,"attached":%s,"blocked":%s}}}' % (
        str(running).lower(), str(running and not other_version).lower(), str(managed).lower(),
        str(attached).lower(), blocked if other_version else "null")
    if stop is None:
        stop = 'echo "%s stop" >> "%s"; ' % (version, log) + (
            """echo '{"stopped":true}'""" if stop_ok else "exit 1")
    start = "echo '{}'" if start_ok else "exit 1"
    return f"""#!/bin/sh
case "$*" in
  --version) echo '{{"version":"{version}"}}' ;;
  *"status --local") echo '{serve}' ;;
  *" stop") {stop} ;;
  *"serve -d --again") echo "{version} serve -d --again" >> "{log}"; {start} ;;
  *) exit 2 ;;
esac
"""


class HostUpgrade(unittest.TestCase):
    """A managed host running the latch being replaced is stopped first and
    started again with the new one."""

    def upgrade(self, old, new, log, engines_fail=False):
        parent = ROOT / ".dev/tests"
        parent.mkdir(parents=True, exist_ok=True)
        directory = tempfile.TemporaryDirectory(dir=parent)
        self.addCleanup(directory.cleanup)
        root = Path(directory.name)
        state = root / "state"
        setup, binary, tools = fixture(root, state, new)
        if engines_fail:
            tool(setup, "browser-engines.sh", "exit 1\n")
        self.recovery = (f"'{root}/home/.local/bin/latch' --state-dir '{state}' "
                         "serve -d (plus any --deny-exec or --headless-browsers flags you need)")
        dest = root / "home/.local/bin/latch"
        dest.parent.mkdir(parents=True)
        dest.write_text(old)
        dest.chmod(0o755)
        env = {**os.environ, "HOME": str(root / "home"), "PATH": f"{tools}:{os.environ['PATH']}"}
        result = subprocess.run(["sh", str(setup / "install-binary.sh"), "--state-dir", str(state),
                                 str(binary)], cwd=root, env=env, capture_output=True, text=True)
        calls = log.read_text().splitlines() if log.exists() else []
        return result, calls, dest

    def log(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        return Path(directory.name) / "calls"

    def test_a_managed_host_is_stopped_then_started_again_by_the_new_latch(self):
        log = self.log()
        result, calls, dest = self.upgrade(fake_latch("0.1.0 (1)", log), fake_latch("0.1.0 (2)", log), log)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(calls, ["0.1.0 (2) stop", "0.1.0 (2) serve -d --again"])
        self.assertIn("started the host", result.stdout)
        self.assertIn("it starts again with the new latch once the install is done",
                      result.stdout, "the stop says it will start again after installation")
        self.assertNotIn("is stopped", result.stderr)
        self.assertIn('"0.1.0 (2)"', subprocess.check_output([str(dest), "--version"], text=True))

    def mac_upgrade(self, old_version, new_version, log, old_window="", new_window=""):
        """Installs an app whose latch says `new_version` over one whose
        latch says `old_version`, with no latch in the bin folder (the link
        is in another one, or none is)."""
        parent = ROOT / ".dev/tests"
        parent.mkdir(parents=True, exist_ok=True)
        directory = tempfile.TemporaryDirectory(dir=parent)
        self.addCleanup(directory.cleanup)
        root = Path(directory.name)
        state = root / "state"
        home = root / "home"
        setup, _, tools = fixture(root, state, "", system="Darwin", machine="arm64")
        (setup / "latch").unlink()
        # Chrome for Testing already in place, so nothing is downloaded.
        cft_pin = '{\n  "version": "1.0"\n}\n'
        (setup / "src/browser").mkdir(parents=True)
        (setup / "src/browser/cft.json").write_text(cft_pin)
        (state / "plugins/cft/1.0").mkdir(parents=True)
        (state / "plugins/cft/1.0/manifest.json").write_text(cft_pin)
        for app, version, window in [(setup / "Plow Latch.app", new_version, new_window),
                                     (home / "Applications/Plow Latch.app", old_version, old_window)]:
            (app / "Contents/MacOS").mkdir(parents=True)
            (app / "Contents/MacOS/latch").write_text(fake_latch(version, log))
            (app / "Contents/MacOS/latch").chmod(0o755)
            (app / "Contents/Frameworks").mkdir()
            (app / "Contents/Frameworks/libPlowLatchUI.dylib").write_text(window)
        (setup / "latch").symlink_to("Plow Latch.app/Contents/MacOS/latch")
        env = {**os.environ, "HOME": str(home), "PATH": f"{tools}:{os.environ['PATH']}"}
        result = subprocess.run(["sh", str(setup / "install-binary.sh"), "--state-dir", str(state),
                                 str(setup / "latch")], cwd=root, env=env, capture_output=True, text=True)
        calls = log.read_text().splitlines() if log.exists() else []
        return result, calls, home / "Applications/Plow Latch.app"

    def test_a_replaced_mac_app_restarts_its_host_found_through_the_app(self):
        """On macOS the app is what gets replaced, a new latch or only a new
        window: its host is stopped and started again even when no latch is
        in the bin folder."""
        for old, before, after in [("0.1.0 (1)", "", ""), ("0.1.0 (2)", "window-one\n", "window-two\n")]:
            with self.subTest(old=old, window=after):
                log = self.log()
                result, calls, app = self.mac_upgrade(old, "0.1.0 (2)", log, before, after)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(calls, ["0.1.0 (2) stop", "0.1.0 (2) serve -d --again"])
                self.assertIn('"0.1.0 (2)"', subprocess.check_output(
                    [str(app / "Contents/MacOS/latch"), "--version"], text=True))
                self.assertEqual((app / "Contents/Frameworks/libPlowLatchUI.dylib").read_text(), after)

    def test_a_failed_stop_changes_nothing(self):
        log = self.log()
        old = fake_latch("0.1.0 (1)", log)
        result, calls, dest = self.upgrade(old, fake_latch("0.1.0 (2)", log, stop_ok=False), log)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("nothing was changed", result.stderr)
        self.assertEqual(dest.read_text(), old)
        self.assertEqual(calls, ["0.1.0 (2) stop"], "tried, and nothing started")

    def test_a_host_that_does_not_start_again_says_how_to_start_it(self):
        log = self.log()
        new = fake_latch("0.1.0 (2)", log, start_ok=False)
        result, calls, _ = self.upgrade(fake_latch("0.1.0 (1)", log), new, log)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(f"is stopped; start once with: {self.recovery}", result.stderr)
        self.assertEqual(calls, ["0.1.0 (2) stop", "0.1.0 (2) serve -d --again"])

    def test_a_later_step_that_fails_leaves_the_command_that_starts_the_host(self):
        log = self.log()
        result, calls, _ = self.upgrade(fake_latch("0.1.0 (1)", log), fake_latch("0.1.0 (2)", log),
                                        log, engines_fail=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(f"is stopped; start once with: {self.recovery}", result.stderr)
        self.assertEqual(calls, ["0.1.0 (2) stop"])

    def test_a_host_in_a_terminal_is_stopped_and_the_owner_told_to_serve_again(self):
        # `serve --foreground`, then an attached `latch serve`.
        for managed, attached in [(False, False), (True, True)]:
            log = self.log()
            new = fake_latch("0.1.0 (2)", log, managed=managed, attached=attached)
            result, calls, _ = self.upgrade(fake_latch("0.1.0 (1)", log), new, log)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(calls, ["0.1.0 (2) stop"], "never started again in the background")
            self.assertIn("ran in a terminal and was stopped: run `latch serve` there again",
                          result.stdout)

    def test_a_host_the_new_latch_cannot_reach_is_left_to_its_owner_and_the_install_goes_on(self):
        # An older protocol's host (build 121's) runs on. The latch there is
        # 132, whose stop never reaches it (here: it fails), or 121, which
        # has no stop at all. Neither is asked.
        stuck = 'echo "{version} stop" >> "{log}"; sleep 1; exit 1'
        unknown = "echo \"error: unrecognized subcommand 'stop'\" >&2; exit 2"
        for installed, stop in [("0.1.0 (132)", stuck), ("0.1.0 (121)", unknown)]:
            log = self.log()
            old = fake_latch(installed, log, stop=stop.format(version=installed, log=log))
            new = fake_latch("0.1.0 (2)", log, managed=False, other_version=True)
            result, calls, dest = self.upgrade(old, new, log)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(calls, [], "nothing was stopped, by either latch")
            self.assertIn("install-binary.sh: a host from another latch version\n", result.stdout)
            self.assertIn('"0.1.0 (2)"',
                          subprocess.check_output([str(dest), "--version"], text=True))

    def test_no_host_or_the_same_version_stops_nothing(self):
        for old_version, running in [("0.1.0 (1)", False), ("0.1.0 (2)", True)]:
            log = self.log()
            old = fake_latch(old_version, log)
            new = fake_latch("0.1.0 (2)", log, running=running)
            result, calls, _ = self.upgrade(old, new, log)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(calls, [])


class MessagesReaderInstall(unittest.TestCase):
    def test_a_mac_install_puts_the_reader_in_this_latchs_plugin_folder(self):
        parent = ROOT / ".dev/tests"
        parent.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=parent) as directory:
            root = Path(directory)
            state = root / "state"
            # What a latch says to --version: one line of JSON.
            script = '#!/bin/sh\necho \'{"version":"9.9.9 (fixture)"}\'\n'
            setup, _, tools = fixture(root, state, script, system="Darwin", machine="arm64")
            (setup / "latch-messages").write_bytes(b"reader-fixture")
            # A Mac install also puts Chrome for Testing in place: one already there.
            pin = '{\n  "version": "1.0"\n}\n'
            (setup / "src/browser").mkdir()
            (setup / "src/browser/cft.json").write_text(pin)
            (state / "plugins/cft/1.0").mkdir(parents=True)
            (state / "plugins/cft/1.0/manifest.json").write_text(pin)
            args = ["sh", str(setup / "install-binary.sh"), "--state-dir", str(state),
                    "--bin-dir", str(root / "bin")]
            env = {**os.environ, "HOME": str(root / "home"), "PATH": f"{tools}:{os.environ['PATH']}"}
            result = subprocess.run(args, cwd=root, env=env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            reader = state / "plugins/messages/9.9.9/latch-messages"
            self.assertEqual(reader.read_bytes(), b"reader-fixture")
            self.assertTrue(os.access(reader, os.X_OK))


def link_basic_tools(folder):
    """The everyday tools the installers use, linked into `folder`: as a
    PATH of its own it has no package manager, unzip, Xvfb or bwrap. Its
    cat reads the kernel's AppArmor user-namespace setting as 0, so no
    AppArmor profile is loaded, whatever the machine running the test says."""
    for name in ["sh", "dirname", "sed", "cksum", "mkdir", "mktemp", "cp", "mv", "rm",
                 "cut", "id", "chmod", "grep", "tr", "sha256sum", "shasum", "perl"]:
        found = shutil.which(name)
        if found:
            (folder / name).symlink_to(found)
    tool(folder, "cat", f"""case "$1" in
  /proc/sys/kernel/apparmor_restrict_unprivileged_userns) echo 0 ;;
  *) exec "{shutil.which("cat")}" "$@" ;;
esac
""")


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
        shutil.copyfile(ROOT / "latch/install-packages.sh", setup / "install-packages.sh")
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

    def chrome_fixture(self, tools, machine="aarch64", key="linux_arm64", folder="chrome-linux-arm64"):
        """A browser-engines.sh beside a pin for a local CfT archive, on a
        Linux machine whose curl serves only that archive. Returns the
        script's folder, the archive, and the file listing each download."""
        setup = self.root / "engines"
        (setup / "src/browser").mkdir(parents=True)
        shutil.copyfile(ROOT / "latch/browser-engines.sh", setup / "browser-engines.sh")
        shutil.copyfile(ROOT / "latch/install-packages.sh", setup / "install-packages.sh")
        archive = self.root / "cft.zip"
        with zipfile.ZipFile(archive, "w") as zip_file:
            zip_file.writestr(f"{folder}/chrome", "#!/bin/sh\n")
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        # The pin's fixed shape, as browser-engines.sh reads it with sed.
        (setup / "src/browser/cft.json").write_text(f"""{{
  "version": "1.0.0",
  "builds": {{
    "{key}": {{
      "url": "https://example.invalid/cft.zip",
      "zip_sha256": "{digest}",
      "folder": "{folder}"
    }}
  }}
}}
""")
        tool(tools, "uname", f'case "$1" in -s) echo Linux;; -m) echo {machine};; esac\n')
        tool(tools, "ldconfig", "printf '%s\\n' 'libgtk-3.so.0 present' 'libasound.so.2 present' 'libnss3.so present' 'libgbm.so.1 present'\n")
        fetched = self.root / "fetched"
        # Serves the fixture for the pinned URL; anything else fails.
        tool(tools, "curl", f"""out=""; url=""
while [ $# -gt 0 ]; do case "$1" in -o) out=$2; shift 2;; --proto) shift 2;; -*) shift;; *) url=$1; shift;; esac; done
[ "$url" = https://example.invalid/cft.zip ] || exit 22
echo "$url" >> "{fetched}"
cp "{archive}" "$out"
""")
        return setup, archive, fetched

    def test_linux_x86_64_chrome_installs_its_pinned_build(self):
        setup, _, fetched = self.chrome_fixture(self.tools, "x86_64", "linux_x64", "chrome-linux64")
        tool(self.tools, "Xvfb", "exit 0\n")
        tool(self.tools, "ldconfig", "printf '%s\\n' 'libgtk-3.so.0 present' 'libasound.so.2 present' 'libgbm.so.1 present'\n")
        log = self.root / "packages"
        tool(self.tools, "id", "echo 0\n")
        tool(self.tools, "apt-get", f'echo "$*" >> "{log}"\n')
        state = self.root / "state"
        result = subprocess.run(
            ["sh", str(setup / "browser-engines.sh"), "--state-dir", str(state), "chrome"],
            env=self.env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((state / "plugins/cft/1.0.0/chrome-linux64/chrome").is_file())
        self.assertIn("install -y -q libnss3", log.read_text())
        self.assertEqual(fetched.read_text().count("\n"), 1)

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

    def test_camoufox_installs_each_platform_and_refuses_an_unpinned_archive(self):
        for system, machine, key in [("Linux", "aarch64", "linux_arm64"),
                                     ("Linux", "x86_64", "linux_x64"),
                                     ("Darwin", "arm64", "mac_arm64"),
                                     ("Darwin", "x86_64", "mac_x64")]:
            with self.subTest(platform=key), tempfile.TemporaryDirectory(dir=self.root) as folder:
                root = Path(folder)
                setup = root / "setup"
                (setup / "src/browser").mkdir(parents=True)
                for name in ["browser-engines.sh", "install-packages.sh"]:
                    shutil.copyfile(ROOT / "latch" / name, setup / name)
                shutil.copytree(ROOT / "latch/camoufox", setup / "camoufox")
                archive = root / "camoufox.zip"
                executable = "camoufox-bin" if system == "Linux" else "Camoufox.app/Contents/MacOS/camoufox"
                with zipfile.ZipFile(archive, "w") as zipped:
                    zipped.writestr(executable, "pinned-camoufox-fixture")
                # A Node stand-in records npm's arguments without running packages.
                node = root / "node-fixture/bin"
                node.mkdir(parents=True)
                npm = root / "npm-called"
                tool(node, "node", f'echo "$*" > "{npm}"\nmkdir node_modules\n')
                node_archive = root / "node.tar.gz"
                with tarfile.open(node_archive, "w:gz") as tar:
                    tar.add(node.parent, arcname="node-fixture")
                build = {
                    "camoufox": {"url": "https://example.invalid/camoufox.zip",
                                  "sha256": hashlib.sha256(archive.read_bytes()).hexdigest()},
                    "node": {"url": "https://example.invalid/node.tar.gz", "folder": "node-fixture",
                             "sha256": hashlib.sha256(node_archive.read_bytes()).hexdigest()},
                }
                pin = setup / "src/browser/camoufox.json"
                pin.write_text(json.dumps({"version": "1.0-beta.1", "builds": {key: build}}, indent=2))
                tools = root / "tools"
                tools.mkdir()
                tool(tools, "uname", f'case "$1" in -s) echo {system};; -m) echo {machine};; esac\n')
                tool(tools, "ldconfig", "printf '%s\\n' 'libgtk-3.so.0 ready' 'libasound.so.2 ready' 'libX11-xcb.so.1 ready' 'libdbus-1.so.3 ready'\n")
                tool(tools, "Xvfb", "exit 0\n")
                tool(tools, "ditto", 'exec unzip -q "$3" -d "$4"\n')
                tool(tools, "curl", f"""out=""; url=""
while [ $# -gt 0 ]; do case "$1" in -o) out=$2; shift 2;; --proto) shift 2;; -*) shift;; *) url=$1; shift;; esac; done
case "$url" in
  https://example.invalid/camoufox.zip) cp "{archive}" "$out" ;;
  https://example.invalid/node.tar.gz) cp "{node_archive}" "$out" ;;
  *) exit 22 ;;
esac
""")
                packages = root / "packages"
                if key == "linux_arm64":
                    tool(tools, "ldconfig", "exit 0\n")
                    tool(tools, "id", "echo 1000\n")
                    tool(tools, "apt-cache", "exit 0\n")
                    tool(tools, "apt-get", f'echo "$*" >> "{packages}"\n')
                    tool(tools, "sudo", f'echo sudo >> "{packages}"\nexec "$@"\n')
                state = root / "state"
                env = {**os.environ, "HOME": str(root / "home"), "PATH": f"{tools}:{os.environ['PATH']}"}
                command = ["sh", str(setup / "browser-engines.sh"), "--state-dir", str(state), "camoufox"]
                first = subprocess.run(command, env=env, capture_output=True, text=True)
                self.assertEqual(first.returncode, 0, first.stderr)
                if key == "linux_arm64":
                    self.assertEqual(packages.read_text().splitlines().count("sudo"), 1)
                    self.assertIn("libgtk-3-0t64 libasound2t64 libx11-xcb1 libdbus-1-3", packages.read_text())
                installed = state / "plugins/camoufox/1.0-beta.1"
                self.assertEqual((installed / "camoufox" / executable).read_text(), "pinned-camoufox-fixture")
                self.assertEqual((installed / "camoufox/version.json").read_text(),
                                 '{"version":"1.0","build":"beta.1"}')
                self.assertEqual((installed / "manifest.json").read_bytes(), pin.read_bytes())
                self.assertIn("ci --ignore-scripts --omit=dev --no-audit --no-fund", npm.read_text())
                self.assertFalse((root / "home/.cache/camoufox").exists())
                self.assertEqual(list(installed.glob(".download.*")), [])
                again = subprocess.run(command, env=env, capture_output=True, text=True)
                self.assertEqual(again.returncode, 0, again.stderr)
                self.assertIn("already installed", again.stdout)
                shutil.rmtree(state)
                archive.write_bytes(b"not the pinned archive")
                bad = subprocess.run(command, env=env, capture_output=True, text=True)
                self.assertNotEqual(bad.returncode, 0)
                self.assertIn("not the pinned", bad.stderr)
                self.assertFalse((installed / "manifest.json").exists())
                self.assertEqual(list(installed.glob(".download.*")), [])

    def test_a_linux_install_without_bubblewrap_installs_it_as_root(self):
        setup = self.root / "setup"
        setup.mkdir()
        for name in ["install-binary.sh", "install-packages.sh"]:
            shutil.copyfile(ROOT / "latch" / name, setup / name)
        tool(setup, "latch", "echo installed-fixture\n")
        state = self.root / "state"
        seed_gog(setup, state)
        log = self.root / "root-log"
        tool(self.tools, "uname", 'case "$1" in -s) echo Linux;; -m) echo x86_64;; esac\n')
        tool(self.tools, "sudo", f'echo "sudo $*" >> "{log}"\nexec "$@"\n')
        tool(self.tools, "apt-get", f'echo "apt-get $*" >> "{log}"\n')
        link_basic_tools(self.tools)
        env = {**self.env, "PATH": str(self.tools)}
        result = subprocess.run(
            ["sh", str(setup / "install-binary.sh"), "--state-dir", str(state), "--no-browser",
             str(setup / "latch")], env=env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("installing as root: bubblewrap", result.stdout)
        self.assertIn("apt-get install -y -q bubblewrap", log.read_text())

    def test_chrome_set_up_after_no_browser_installs_unzip_and_xvfb_first(self):
        # A Linux x86_64 host with neither unzip nor Xvfb: only the tools named here are on
        # its PATH, and its package manager installs the two.
        tools = self.root / "linux"
        tools.mkdir()
        link_basic_tools(tools)
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
        setup, _, fetched = self.chrome_fixture(tools, "x86_64", "linux_x64", "chrome-linux64")
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
        self.assertTrue((tools / "Xvfb").exists())
        self.assertTrue((state / "plugins/cft/1.0.0/manifest.json").is_file())
        self.assertEqual(fetched.read_text().count("\n"), 1)

        again = setup_chrome()
        self.assertEqual(again.returncode, 0, again.stderr)
        installs = [line for line in log.read_text().splitlines() if line.startswith("apt-get install")]
        self.assertEqual(len(installs), 1, "installed once")


if __name__ == "__main__":
    unittest.main()

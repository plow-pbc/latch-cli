"""Exercise release selection, verification, and setup without network or user state.

Each OS checks a release's signature its own way: a Mac with the release's
own latch (LATCH_RELEASE_LATCH, the one just built), Linux with openssl,
Windows (install.ps1) with its own check. So each runs the tests of its own
targets."""
import base64
import functools
import hashlib
import http.server
import io
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import tarfile
import tempfile
import threading
import unittest
import zipfile

ROOT = Path(__file__).resolve().parents[1]
SYSTEM = platform.system()
WINDOWS = SYSTEM == "Windows"
INSTALLER = ROOT / ("install.ps1" if WINDOWS else "install.sh")
TARGETS = {
    "Darwin": [("arm64", "aarch64-apple-darwin"), ("x86_64", "x86_64-apple-darwin")],
    "Linux": [("x86_64", "x86_64-unknown-linux-musl"), ("aarch64", "aarch64-unknown-linux-musl")],
    "Windows": [("AMD64", "x86_64-pc-windows-msvc")],
}[SYSTEM]

# Ed25519 (RFC 8032), only to sign as minisign does with a key made for the tests.
P = 2**255 - 19
L = 2**252 + 27742317777372353535851937790883648493
D = -121665 * pow(121666, P - 2, P) % P
GX = 15112221349535400772501151409588531511454012693041857206046113283949847762202
GY = 46316835694926478169428394003475163141307993866256225615783033603165251855960
G = (GX, GY, 1, GX * GY % P)


def add(a, b):
    e, f = (a[1] - a[0]) * (b[1] - b[0]) % P, (a[1] + a[0]) * (b[1] + b[0]) % P
    g, h = 2 * a[3] * b[3] * D % P, 2 * a[2] * b[2] % P
    e, f, g, h = f - e, h - g, h + g, f + e
    return (e * f % P, g * h % P, f * g % P, e * h % P)


def mul(scalar, point):
    out = (0, 1, 1, 0)
    while scalar:
        if scalar & 1:
            out = add(out, point)
        point, scalar = add(point, point), scalar >> 1
    return out


def encode(point):
    inverse = pow(point[2], P - 2, P)
    x, y = point[0] * inverse % P, point[1] * inverse % P
    return (y | (x & 1) << 255).to_bytes(32, "little")


def number(data):
    return int.from_bytes(hashlib.sha512(data).digest(), "little")


class Key:
    """A minisign key pair: its public key line, and signatures as minisign writes them."""

    def __init__(self, seed):
        digest = hashlib.sha512(seed).digest()
        self.secret = (int.from_bytes(digest[:32], "little") & (1 << 254) - 8) | 1 << 254
        self.prefix = digest[32:]
        self.point = encode(mul(self.secret, G))
        self.id = hashlib.sha256(seed).digest()[:8]
        self.public = base64.b64encode(b"Ed" + self.id + self.point).decode()

    def ed25519(self, message):
        r = number(self.prefix + message) % L
        big_r = encode(mul(r, G))
        s = (r + number(big_r + self.point + message) * self.secret) % L
        return big_r + s.to_bytes(32, "little")

    def sign(self, data, comment="latch v0.1.0"):
        signature = self.ed25519(hashlib.blake2b(data).digest())
        whole = self.ed25519(signature + comment.encode())
        return (f"untrusted comment: test\n{base64.b64encode(b'ED' + self.id + signature).decode()}\n"
                f"trusted comment: {comment}\n{base64.b64encode(whole).decode()}\n")


RELEASE = Key(b"release key")
OTHER = Key(b"another key")


@functools.cache
def served(root):
    """A local server for `root`'s files, as releases.plow.co/latch-cli
    serves a release: its base URL. For install.ps1, which downloads with
    PowerShell rather than a curl the tests can stand in for."""
    class Quiet(http.server.SimpleHTTPRequestHandler):
        def log_message(self, *_):
            pass

    handler = functools.partial(Quiet, directory=root)
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return f"http://127.0.0.1:{server.server_address[1]}"


class ReleaseInstall(unittest.TestCase):
    def run_install(self, machine, target, corrupt=False, relative_state=False,
                    relative_bin=False, release="v0.1.0", client_only=False, sums_key=RELEASE,
                    latest_key=RELEASE, sums_signed=True, app_signed=True, smuggled=None):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            caller = root / "caller"
            caller.mkdir()
            state_arg = "profiles/team state" if relative_state else str(root / "state")
            expected_state = caller / state_arg
            tools = root / "bin"
            tools.mkdir()
            # The installer with the tests' key in place of the release key.
            installer = root / INSTALLER.name
            if WINDOWS:
                # Served as releases.plow.co would serve it: from `root`, at
                # /latch-cli/... and /latch-cli/releases/<tag>/... alike.
                serve = root / "served"
                text = re.sub(r"(?m)^\$releaseKey = .*$", f"$releaseKey = '{RELEASE.public}'",
                              INSTALLER.read_text())
                text = re.sub(r"(?m)^\$releases = .*$", f"$releases = '{served(str(serve))}'", text)
                installer.write_text(text)
            else:
                installer.write_text(re.sub(r"(?m)^release_key=.*$", f"release_key={RELEASE.public}",
                                            INSTALLER.read_text()))
            folder = f"latch-v0.1.0-{target}"
            archive = root / (f"{folder}.zip" if WINDOWS else f"{folder}.tar.gz")
            if WINDOWS:
                with zipfile.ZipFile(archive, "w") as zip:
                    zip.writestr(f"{folder}/install-binary.ps1", r'''
$args | Set-Content -Encoding utf8 $env:RESULT
New-Item -ItemType Directory -Force $args[1] | Out-Null
New-Item -ItemType File -Force (Join-Path $args[1] 'setup-marker') | Out-Null
if ($args[2] -eq '--bin-dir') {
  New-Item -ItemType Directory -Force $args[3] | Out-Null
  New-Item -ItemType File -Force (Join-Path $args[3] 'bin-marker') | Out-Null
}
''')
                    # The latch just built, which install.ps1 asks its version.
                    zip.write(os.environ["LATCH_RELEASE_LATCH"], f"{folder}/latch.exe")
                    for name, data in (smuggled or {}).items():
                        zip.writestr(name, data)
            else:
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
                    # Files at the archive's top level, beside the release's own downloads.
                    for name, data in (smuggled or {}).items():
                        info = tarfile.TarInfo(name)
                        info.size = len(data)
                        tar.addfile(info, io.BytesIO(data))
                    if SYSTEM == "Darwin":
                        app = tarfile.TarInfo(f"{folder}/Plow Latch.app/Contents/MacOS/latch")
                        app.type, app.linkname = tarfile.SYMTYPE, os.environ["LATCH_RELEASE_LATCH"]
                        tar.addfile(app)
            digest = "0" * 64 if corrupt else hashlib.sha256(archive.read_bytes()).hexdigest()
            sums = f"{digest}  {archive.name}\n".encode()
            (root / "SHA256SUMS").write_bytes(sums)
            if sums_signed:
                (root / "SHA256SUMS.minisig").write_text(sums_key.sign(sums))
            latest = b'{"tag":"v0.1.0","build":1}\n'
            (root / "latest.json").write_bytes(latest)
            (root / "latest.json.minisig").write_text(latest_key.sign(latest))
            if WINDOWS:
                (serve / "releases/v0.1.0").mkdir(parents=True)
                for name in ["latest.json", "latest.json.minisig"]:
                    shutil.copy(root / name, serve / name)
                for name in [archive.name, "SHA256SUMS", "SHA256SUMS.minisig"]:
                    if (root / name).exists():
                        shutil.copy(root / name, serve / "releases/v0.1.0" / name)
            # curl, serving the release from FIXTURE as releases.plow.co would.
            (tools / "curl").write_text('''#!/bin/sh
out=""
url=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    -o | --proto) [ "$1" = -o ] && out=$2; shift 2 ;;
    -*) shift ;;
    *) url=$1; shift ;;
  esac
done
case "$url" in
  https://releases.plow.co/latch-cli/latest.json*) cp "$FIXTURE/${url##*/}" "$out" ;;
  https://releases.plow.co/latch-cli/releases/v0.1.0/*) cp "$FIXTURE/${url##*/}" "$out" ;;
  *) exit 22 ;;
esac
''')
            (tools / "uname").write_text(
                f'#!/bin/sh\ncase "$1" in -s) echo {SYSTEM};; -m) echo {machine};; esac\n'
            )
            # codesign and Gatekeeper, as they judge the fixture's app.
            for tool in ["codesign", "spctl"]:
                (tools / tool).write_text(f"#!/bin/sh\nexit {0 if app_signed else 1}\n")
            for tool in tools.iterdir():
                tool.chmod(0o755)
            result_file = root / "result"
            options = ["--state-dir", state_arg]
            if relative_bin:
                options += ["--bin-dir", "apps/team tools"]
            if client_only:
                options += ["--client-only"]
            shell = ["powershell.exe", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File"] if WINDOWS else ["sh"]
            result = subprocess.run(
                [*shell, str(installer), *([release] if release else []), *options],
                cwd=caller,
                env={**os.environ, "PATH": f"{tools}:{os.environ['PATH']}",
                     "FIXTURE": str(root), "RESULT": str(result_file)},
                capture_output=True, text=True,
            )
            if result.returncode == 0:
                arguments = result_file.read_text(encoding="utf-8-sig").splitlines()
                self.assertEqual(arguments[:-1], options)
                self.assertEqual(Path(arguments[-1]).name, "latch.exe" if WINDOWS else "latch")
                self.assertTrue((expected_state / "setup-marker").exists(),
                                "setup must use the caller's state directory, surviving cleanup")
                # Windows' fixture latch is the one just built: its version.
                self.assertIn('"version"' if WINDOWS else "latch 0.1.0 fixture", result.stdout)
                if relative_bin:
                    self.assertTrue((caller / "apps/team tools/bin-marker").exists())
                # Only a host's install mentions gog and bubblewrap.
                for host_only in ["gog"] if WINDOWS else ["gog", "bubblewrap"]:
                    self.assertEqual(host_only in result.stdout, not client_only, host_only)
            else:
                self.assertFalse(result_file.exists(), "setup ran before the release was verified")
            return result

    def install(self, **options):
        machine, target = TARGETS[0]
        return self.run_install(machine, target, **options)

    def assertRefused(self, result, why):
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn(why, result.stderr)

    def test_all_targets_and_setup_arguments(self):
        for machine, target in TARGETS:
            with self.subTest(target=target):
                result = self.run_install(machine, target)
                self.assertEqual(result.returncode, 0, result.stderr)

    def test_relative_state_directory_survives_download_cleanup(self):
        result = self.install(relative_state=True)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_latest_installs_the_newest_published_release(self):
        result = self.install(release="latest")
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_no_tag_installs_the_newest_published_release(self):
        result = self.install(release=None)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_relative_bin_directory_survives_download_cleanup(self):
        result = self.install(relative_bin=True)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_client_only_goes_on_to_install_binary_and_mentions_no_host_tools(self):
        result = self.install(client_only=True)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_bad_checksum_never_runs_setup(self):
        why = "does not match SHA256SUMS" if WINDOWS else "computed checksum did NOT match"
        self.assertRefused(self.install(corrupt=True), why)

    def test_sums_signed_with_another_key_never_run_setup(self):
        for client_only in [False, True]:
            with self.subTest(client_only=client_only):
                result = self.install(sums_key=OTHER, client_only=client_only)
                self.assertRefused(result, "SHA256SUMS is not signed with latch's release key")

    def test_an_archive_carrying_a_signed_sums_of_its_own_never_runs_setup(self):
        # A genuine SHA256SUMS of another release, signed with the release key,
        # inside an archive whose served SHA256SUMS no release key signed.
        genuine = f"{'1' * 64}  latch-v0.0.9-any.tar.gz\n".encode()
        smuggled = {"SHA256SUMS": genuine, "SHA256SUMS.minisig": RELEASE.sign(genuine).encode()}
        result = self.install(sums_key=OTHER, smuggled=smuggled)
        self.assertRefused(result, "SHA256SUMS is not signed with latch's release key")

    def test_unsigned_sums_never_run_setup(self):
        self.assertRefused(self.install(sums_signed=False), "could not download releases/v0.1.0/SHA256SUMS.minisig")

    def test_a_latest_json_signed_with_another_key_never_runs_setup(self):
        result = self.install(release="latest", latest_key=OTHER)
        self.assertRefused(result, "latest.json is not signed with latch's release key")

    # Only a Mac's release carries an app, signed by Plow.
    if SYSTEM == "Darwin":
        def test_an_app_plow_did_not_sign_never_runs(self):
            self.assertRefused(self.install(app_signed=False), "is not one Plow signed")

    def test_the_installers_carry_the_key_latch_update_checks(self):
        update = (ROOT / "latch/src/update.rs").read_text()
        key = re.search(r'const RELEASE_KEY: &str = "([^"]+)"', update).group(1)
        self.assertIn(f"\nrelease_key={key}\n", (ROOT / "install.sh").read_text())
        self.assertIn(f"\n$releaseKey = '{key}'\n", (ROOT / "install.ps1").read_text())


if __name__ == "__main__":
    unittest.main()

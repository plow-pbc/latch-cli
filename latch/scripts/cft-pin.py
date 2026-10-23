"""Print the `builds` of `src/browser/cft.json` for one Chrome for Testing release.

    python3 scripts/cft-pin.py VERSION

Downloads each pinned build of VERSION, takes the zip's sha256 (what
install.sh checks) and, after extracting it as install.sh does, the tree
hash latch checks before every launch (`browser::engine::tree_hash`): with
`ditto` for the macOS builds and `unzip` for Linux's, as install.sh does on
each. Run it on a Mac, which has both.
"""

import hashlib
import json
import os
import subprocess
import sys
import tempfile
import urllib.request
from pathlib import Path

MAC_EXECUTABLE = "Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing"
# Pin key: CfT's platform name, how install.sh unpacks it there, and the program.
PLATFORMS = {
    "mac_arm64": ("mac-arm64", ["ditto", "-x", "-k"], MAC_EXECUTABLE),
    "mac_x64": ("mac-x64", ["ditto", "-x", "-k"], MAC_EXECUTABLE),
    "linux_arm64": ("linux-arm64", ["unzip", "-q"], "chrome"),
    "linux_x64": ("linux64", ["unzip", "-q"], "chrome"),
}
URL = "https://storage.googleapis.com/chrome-for-testing-public/{version}/{platform}/chrome-{platform}.zip"


def file_sha256(path):
    digest = hashlib.sha256()
    with open(path, "rb") as file:
        for block in iter(lambda: file.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def tree_hash(root):
    """sha256 over one line per entry below `root`, sorted by relative path
    bytes: `d PATH`, `l PATH\\0TARGET`, or `f PATH\\0x|-\\0SHA256` (x when
    any execute bit is set), each ending in a newline."""
    root = Path(root)
    entries = []
    for dirpath, dirnames, filenames in os.walk(root):
        for name in dirnames + filenames:
            path = Path(dirpath) / name
            entries.append(os.fsencode(path.relative_to(root)))
    digest = hashlib.sha256()
    for rel in sorted(entries):
        path = root / os.fsdecode(rel)
        info = path.lstat()
        if path.is_symlink():
            digest.update(b"l " + rel + b"\0" + os.fsencode(os.readlink(path)) + b"\n")
        elif path.is_dir():
            digest.update(b"d " + rel + b"\n")
        elif path.is_file():
            mode = b"x" if info.st_mode & 0o111 else b"-"
            digest.update(b"f " + rel + b"\0" + mode + b"\0" + file_sha256(path).encode() + b"\n")
        else:
            raise SystemExit(f"{path}: not a file, folder or link")
    return digest.hexdigest()


def main():
    if len(sys.argv) != 2:
        raise SystemExit(__doc__)
    version = sys.argv[1]
    builds = {}
    with tempfile.TemporaryDirectory() as tmp:
        for key, (platform, unpack, executable) in PLATFORMS.items():
            zip_path = Path(tmp) / f"chrome-{platform}.zip"
            urllib.request.urlretrieve(URL.format(version=version, platform=platform), zip_path)
            out = Path(tmp) / key
            if unpack[0] == "unzip":
                subprocess.run([*unpack, str(zip_path), "-d", str(out)], check=True)
            else:
                subprocess.run([*unpack, str(zip_path), str(out)], check=True)
            folder = f"chrome-{platform}"
            builds[key] = {
                "url": URL.format(version=version, platform=platform),
                "zip_sha256": file_sha256(zip_path),
                "folder": folder,
                "tree_sha256": tree_hash(out / folder),
                "executable": executable,
            }
            # On Linux arm64 this one crashes the network service at once.
            if key.startswith("mac_"):
                builds[key]["args"] = ["--disable-crashpad-for-testing"]
    print(json.dumps(builds, indent=2))


if __name__ == "__main__":
    main()

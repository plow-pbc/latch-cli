"""Print the `builds` of `src/browser/chromium.json` for one Debian Chromium release.

    python3 scripts/chromium-pin.py VERSION

VERSION is the Debian package version (155.0.8059.79-1~deb13u1). Finds the
arm64 chromium and chromium-common packages of VERSION on
snapshot.debian.org, whose URLs never change, downloads them, takes each
one's sha256 (what install.sh checks) and, after unpacking both with
`dpkg-deb -x` as install.sh does, the tree hash latch checks before every
launch (`browser::engine::tree_hash`) of the folder latch runs it from,
`usr/lib/chromium`. The libraries chromium depends on are listed for
install.sh to install with apt. Run it on Debian, which has dpkg-deb.
"""

import importlib.util
import json
import re
import subprocess
import sys
import tempfile
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("cft_pin", HERE / "cft-pin.py")
cft_pin = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cft_pin)

SNAPSHOT = "https://snapshot.debian.org"
PACKAGES = ["chromium", "chromium-common"]


def deb_url(package, version):
    """The arm64 package's URL on snapshot.debian.org."""
    query = f"{SNAPSHOT}/mr/binary/{package}/{version}/binfiles?fileinfo=1"
    with urllib.request.urlopen(query) as reply:
        found = json.load(reply)
    for row in found["result"]:
        if row["architecture"] == "arm64":
            info = found["fileinfo"][row["hash"]][0]
            return f"{SNAPSHOT}/archive/{info['archive_name']}/{info['first_seen']}{info['path']}/{info['name']}"
    raise SystemExit(f"no arm64 {package} {version} on {SNAPSHOT}")


def libraries(deb):
    """chromium's Depends, each alternative list as its first name, without
    versions or the chromium-common it comes with."""
    depends = subprocess.run(
        ["dpkg-deb", "-f", str(deb), "Depends"], check=True, capture_output=True, text=True
    ).stdout
    names = [re.split(r"[ (]", group.split("|")[0].strip())[0] for group in depends.split(",")]
    return " ".join(name for name in names if name and name != "chromium-common")


def main():
    if len(sys.argv) != 2:
        raise SystemExit(__doc__)
    version = sys.argv[1]
    debs = []
    with tempfile.TemporaryDirectory() as tmp:
        out = Path(tmp) / "x"
        for package in PACKAGES:
            url = deb_url(package, version)
            deb = Path(tmp) / f"{package}.deb"
            urllib.request.urlretrieve(url, deb)
            subprocess.run(["dpkg-deb", "-x", str(deb), str(out)], check=True)
            debs.append({"url": url, "sha256": cft_pin.file_sha256(deb)})
        build = {
            "debs": debs,
            "packages": libraries(Path(tmp) / "chromium.deb"),
            "folder": "chromium",
            "tree_sha256": cft_pin.tree_hash(out / "usr/lib/chromium"),
            "executable": "chromium",
        }
    print(json.dumps({"linux_arm64": build}, indent=2))


if __name__ == "__main__":
    main()

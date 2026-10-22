"""Print the `builds` of `src/browser/camoufox.json` with their tree hashes.

    python3 scripts/camoufox-pin.py

For each macOS build in the pin, downloads Camoufox and Node, checks their
sha256 against the pin, and sets them up as `install.sh --browser camoufox`
does (Camoufox with `ditto` and its version.json, Node with `tar`, the
runtime from `camoufox/` with `npm ci`, for that build's CPU), then takes
the tree hash of each part, which latch checks before every launch
(`browser::engine::tree_hash`). Runs on a Mac whose own build is pinned:
its Node runs `npm ci` for both.
"""

import importlib.util
import json
import os
import platform
import shutil
import subprocess
import sys
import tempfile
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
LATCH = HERE.parent
spec = importlib.util.spec_from_file_location("cft_pin", HERE / "cft-pin.py")
cft_pin = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cft_pin)

CPU = {"mac_arm64": "arm64", "mac_x64": "x64"}
RUNTIME_FILES = ["package.json", "package-lock.json", "sqlite"]


def version_json(version):
    """The version.json camoufox-js reads, byte for byte as install.sh writes it."""
    number, build = version.split("-", 1)
    return f'{{"version":"{number}","build":"{build}"}}'


def fetch(url, sha256, into):
    urllib.request.urlretrieve(url, into)
    got = cft_pin.file_sha256(into)
    if got != sha256:
        raise SystemExit(f"{url} has sha256 {got}, not the pinned {sha256}")


def main():
    pin = json.loads((LATCH / "src/browser/camoufox.json").read_text())
    host = "mac_arm64" if platform.machine() == "arm64" else "mac_x64"
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        for key, build in pin["builds"].items():
            root = tmp / key
            (root / "camoufox").mkdir(parents=True)
            fetch(build["camoufox"]["url"], build["camoufox"]["sha256"], tmp / f"{key}.zip")
            subprocess.run(["ditto", "-x", "-k", str(tmp / f"{key}.zip"), str(root / "camoufox")], check=True)
            (root / "camoufox/version.json").write_text(version_json(pin["version"]))
            fetch(build["node"]["url"], build["node"]["sha256"], tmp / f"{key}.tar.gz")
            subprocess.run(["tar", "-xzf", str(tmp / f"{key}.tar.gz"), "-C", str(tmp)], check=True)
            os.rename(tmp / build["node"]["folder"], root / "node")
        host_node = tmp / host / "node"
        for key, build in pin["builds"].items():
            runtime = tmp / key / "runtime"
            runtime.mkdir()
            for name in RUNTIME_FILES:
                source = LATCH / "camoufox" / name
                if source.is_dir():
                    shutil.copytree(source, runtime / name)
                else:
                    shutil.copy(source, runtime / name)
            subprocess.run(
                [
                    str(host_node / "bin/node"),
                    str(host_node / "lib/node_modules/npm/bin/npm-cli.js"),
                    "ci",
                    "--ignore-scripts",
                    "--omit=dev",
                    "--no-audit",
                    "--no-fund",
                    "--os=darwin",
                    f"--cpu={CPU[key]}",
                ],
                cwd=runtime,
                env={
                    "PATH": f"{host_node / 'bin'}:/usr/bin:/bin",
                    "HOME": str(tmp),
                    "npm_config_cache": str(tmp / f"npm-cache-{key}"),
                    "npm_config_update_notifier": "false",
                },
                # npm's report goes with the other notices, not into the pin.
                stdout=sys.stderr,
                check=True,
            )
            build["camoufox"]["tree_sha256"] = cft_pin.tree_hash(tmp / key / "camoufox")
            build["node"]["tree_sha256"] = cft_pin.tree_hash(tmp / key / "node")
            build["runtime_tree_sha256"] = cft_pin.tree_hash(runtime)
    print(json.dumps(pin["builds"], indent=2))


if __name__ == "__main__":
    main()

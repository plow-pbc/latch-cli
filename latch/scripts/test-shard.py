#!/usr/bin/env python3
"""Partition complete libtest inventories while retaining their in-process scheduler."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import time


def partition(binary, names, shard, count):
    return [name for name in names
            if int.from_bytes(hashlib.sha256(f"{binary}::{name}".encode()).digest()[:8], "big") % count == shard - 1]


def inventory(executable):
    listing = subprocess.check_output([executable, "--list", "--format", "terse"], text=True)
    return [line.removesuffix(": test") for line in listing.splitlines() if line.endswith(": test")]


def run(shard, count):
    started = time.monotonic()
    build = subprocess.run(["cargo", "test", "--locked", "--no-run", "--message-format=json"],
                           stdout=subprocess.PIPE, text=True)
    artifacts = {}
    for line in build.stdout.splitlines():
        message = json.loads(line)
        if message.get("reason") == "compiler-message":
            print(message["message"].get("rendered", ""), end="", flush=True)
        if message.get("reason") == "compiler-artifact" and message["profile"]["test"] and message.get("executable"):
            artifacts[message["executable"]] = message["target"]["name"]
    if build.returncode:
        return build.returncode
    if not artifacts:
        raise RuntimeError("cargo produced no test executables")
    report = {"shard": shard, "count": count, "binaries": []}
    status = 0
    for executable, binary in sorted(artifacts.items()):
        names = inventory(executable)
        buckets = [partition(binary, names, index, count) for index in range(1, count + 1)]
        assigned = [name for bucket in buckets for name in bucket]
        if len(assigned) != len(set(assigned)) or set(assigned) != set(names):
            raise RuntimeError(f"incomplete or overlapping partitions: {binary}")
        selected = buckets[shard - 1]
        print(f"{binary}: shard {shard}/{count}, {len(selected)}/{len(names)} tests", flush=True)
        begin = time.monotonic()
        # An empty filter list means 'all tests' to libtest, so never invoke it.
        result = subprocess.run([executable, "--exact", *selected]).returncode if selected else 0
        status = status or result
        report["binaries"].append({"name": binary, "inventory": names, "selected": selected,
                                   "seconds": round(time.monotonic() - begin, 2), "exit": result})
    report["seconds"] = round(time.monotonic() - started, 2)
    output = Path("../.dev/ci")
    output.mkdir(parents=True, exist_ok=True)
    (output / f"shard-{shard}.json").write_text(json.dumps(report, indent=2) + "\n")
    return status


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("shard", type=int)
    parser.add_argument("count", type=int)
    args = parser.parse_args()
    if not 1 <= args.shard <= args.count:
        parser.error("expected 1 <= shard <= count")
    raise SystemExit(run(args.shard, args.count))

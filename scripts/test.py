#!/usr/bin/env python3
"""latch's tests: `just test [SUITE...] [--linux]`.

With no suite, runs `gate`: everything a merge needs on this machine's OS.
`--linux` runs the same plan in the Linux test container (latch/docker/
Dockerfile.test), from a Mac or from Linux. The whole plan is checked before
anything runs, and a selected suite that cannot run here is an error, never
a skip. `just test-list` lists the suites; `just test-plan ...` prints a
plan without running it.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shlex
import shutil
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]
LATCH = ROOT / "latch"
DEV = ROOT / ".dev"
IMAGE = "latch-linux-test:local"

# Each suite: what it covers, what it needs, and where it runs ("macos",
# "linux" on a Linux machine, "container" with --linux).
SUITES = {
    "core": ("unit tests and the CLI's integration tests", "nothing else",
             {"macos", "linux", "container"}),
    "sandbox": ("the real worker under Seatbelt (macOS) or bubblewrap (Linux)", "nothing else",
                {"macos", "linux", "container"}),
    "messages": ("Messages and Contacts: the CLI's tests and the Swift reader's", "macOS, Swift",
                 {"macos"}),
    "browser": ("the real-engine tests (on macOS only those tagged `mac`, and every test on an "
                "engine Linux has no pin for), and the tests that need the pinned gog",
                "the pinned engines, installed into .dev/ first", {"macos", "linux", "container"}),
    "lifecycle": ("a host run by the service manager: launchd, or systemd", "macOS, or Linux "
                  "with a systemd user session", {"macos", "linux"}),
    "scenario": ("the return scenario on the test shop, with the fake gog",
                 "the pinned engines", {"macos", "linux", "container"}),
    "installer": ("install.sh, install-binary.sh and dev-login.py against local fixtures, and "
                  "on macOS the app bundle", "Python 3", {"macos", "linux", "container"}),
}
GATE = {
    "macos": ["core", "sandbox", "messages", "browser", "lifecycle", "installer"],
    "linux": ["core", "sandbox", "browser", "lifecycle", "scenario", "installer"],
    "container": ["core", "sandbox", "browser", "scenario", "installer"],
}
NOT_HERE = {
    ("lifecycle", "container"): "not available in the container (runs in CI and on the Pi)",
    ("messages", "linux"): "macOS only",
    ("messages", "container"): "macOS only",
}
# Cargo features per OS: one build serves every suite.
FEATURES = {"macos": "fake-gog", "linux": "fake-gog,linux-browser-tests,systemd-tests"}
ENGINES = ("cft", "camoufox")


def where_we_are(args):
    if args.linux or os.environ.get("LATCH_TEST_CONTAINER"):
        return "container"
    system = platform.system()
    if system == "Darwin":
        return "macos"
    if system == "Linux":
        return "linux"
    raise SystemExit(f"test.py: no tests run on {system}")


def pin_platform(where):
    """The engine pins' name for the machine the tests run on: the
    container is always linux/arm64, whatever the host."""
    if where == "container":
        return "linux_arm64"
    machine = platform.machine().lower()
    arch = "arm64" if machine in ("arm64", "aarch64") else "x64" if machine in ("x86_64", "amd64") else machine
    return f"{'mac' if where == 'macos' else 'linux'}_{arch}"


def pinned_engines(key):
    pins = {}
    for engine in ENGINES:
        pins[engine] = key in json.loads((LATCH / f"src/browser/{engine}.json").read_text())["builds"]
    return pins


def suite_of(binary, name):
    """The one suite each Rust test belongs to."""
    if binary == "worker":
        return "sandbox"
    if name.startswith("browser::tests::") or "pinned::" in name:
        return "browser"
    head = name.split("::")[0]
    if binary == "cli" and head in ("scenario", "lifecycle", "messages"):
        return head
    return "core"


def runs_on_macos(name):
    """Of the browser suite, what macOS runs: tests tagged `mac`, and every
    test on an engine Linux has no pin for (it would run nowhere else)."""
    parts = name.split("::")
    if "mac" in parts or "pinned" in parts:
        return True
    linux = pinned_engines("linux_arm64")
    return any(engine in parts and not linux[engine] for engine in ENGINES)


def plan(args):
    where = where_we_are(args)
    asked = args.suites or ["gate"]
    unknown = [s for s in asked if s != "gate" and s not in SUITES]
    if unknown:
        raise SystemExit(f"test.py: no suite {', '.join(unknown)}; `just test-list` lists them")
    gate = "gate" in asked
    suites = []
    for suite in GATE[where] if gate else []:
        suites.append(suite)
    for suite in asked:
        if suite != "gate" and suite not in suites:
            suites.append(suite)
    for suite in suites:
        if where not in SUITES[suite][2]:
            reason = NOT_HERE.get((suite, where), f"does not run on {where}")
            raise SystemExit(f"test.py: {suite}: {reason}")
    not_here = [(s, NOT_HERE[(s, where)]) for s in SUITES if s not in suites and (s, where) in NOT_HERE
                and gate]
    if {"browser", "scenario"} & set(suites) and where != "macos":
        key = pin_platform(where)
        if not pinned_engines(key)["cft"]:
            raise SystemExit(f"test.py: browser and scenario need an engine, and none is pinned for "
                             f"{key}; run them on arm64 (CI: ubuntu-24.04-arm) or with --linux "
                             f"from an Apple silicon Mac")
    if "lifecycle" in suites and where == "linux":
        session = subprocess.run(["systemctl", "--user", "show-environment"], capture_output=True)
        if session.returncode:
            raise SystemExit("test.py: lifecycle needs a systemd user session (`systemctl --user` "
                             "answers); on a CI runner, `loginctl enable-linger` first")
    if args.shard:
        try:
            shard, count = (int(n) for n in args.shard.split("/"))
        except ValueError:
            raise SystemExit("test.py: --shard N/COUNT, e.g. --shard 1/2")
        if not 1 <= shard <= count:
            raise SystemExit("test.py: --shard N/COUNT with 1 <= N <= COUNT")
        args.shard = (shard, count)
    return where, suites, gate, not_here


def cargo_env(where):
    env = dict(os.environ)
    if where == "container":
        env["LATCH_CFT_STATE"] = str(DEV / "linux-cft-state")
        env["CARGO_TARGET_DIR"] = str(target_dir(where))
    return env


def engines_state(where):
    return DEV / ("linux-cft-state" if where == "container" else "cft-state")


def steps(where, suites, gate, first_shard):
    """What runs, in order: (name, suites it serves, command, cwd)."""
    features = FEATURES["macos" if where == "macos" else "linux"]
    rust = [s for s in suites if s not in ("installer",)]
    out = []
    if gate and first_shard:
        out.append(("fmt", ["gate"], ["cargo", "fmt", "--check"], LATCH))
        out.append(("clippy", ["gate"], ["cargo", "clippy", "--locked", "--all-targets", "--features",
                                         features, "--", "-D", "warnings"], LATCH))
    if rust:
        out.append(("build", rust, ["cargo", "test", "--locked", "--features", features, "--no-run",
                                    "--message-format=json-render-diagnostics"], LATCH))
    if {"browser", "scenario"} & set(suites):
        state = engines_state(where)
        if where == "macos":
            # The pinned gog too, which macOS's Google tests run; HOME keeps
            # install-binary.sh's copy of latch out of the real ~/.local/bin.
            command = ["env", f"HOME={DEV / 'cft-home'}", "sh", "install-binary.sh", "--non-interactive",
                       "--state-dir", str(state), "--browser", "chrome", "--browser", "camoufox",
                       str(target_dir(where) / "debug/latch")]
        else:
            engines = ["chrome"] + (["camoufox"] if pinned_engines(pin_platform(where))["camoufox"] else [])
            command = ["sh", "browser-engines.sh", "--state-dir", str(state), *engines]
        out.append(("engines", ["browser", "scenario"], command, LATCH))
    if rust:
        out.append(("rust", rust, None, LATCH))
    if "messages" in suites and first_shard:
        out.append(("swift reader", ["messages"], ["env", "LATCH_FUZZ_ITERATIONS=50000", "swift", "test"],
                    LATCH / "messages-reader"))
    if "core" in suites and first_shard:
        out.append(("test.py", ["core"], ["python3", "scripts/test_test.py"], ROOT))
    if "installer" in suites and first_shard:
        out.append(("install.sh", ["installer"], ["python3", "scripts/test-install-release.py"], ROOT))
        out.append(("install-binary.sh", ["installer"], ["python3", "scripts/test-install.py"], ROOT))
        out.append(("dev-login.py", ["installer"], ["python3", "scripts/test-dev-login.py"], LATCH))
        if where == "macos":
            if not rust:
                out.append(("latch", ["installer"], ["cargo", "build", "--locked", "--features", features],
                            LATCH))
            out.append(("app bundle", ["installer"], ["sh", str(ROOT / "scripts/test-app-bundle.sh")], LATCH))
    return out


def target_dir(where):
    if where == "container":
        return LATCH / "target/linux-test"
    return Path(os.environ.get("CARGO_TARGET_DIR", LATCH / "target"))


def show(command):
    """A command as the plan prints it, with paths from the repo root."""
    def short(part):
        part = str(part)
        for prefix in (f"{ROOT}/", f"HOME={ROOT}/"):
            if part.startswith(prefix):
                return part.replace(f"{ROOT}/", "", 1)
        return part
    return " ".join(shlex.quote(short(part)) for part in command)


def print_plan(where, suites, gate, not_here, args):
    here = {"macos": "this Mac", "linux": "this Linux machine", "container": "the Linux test container"}
    print(f"test.py: {'gate: ' if gate else ''}{' '.join(suites)} on {here[where]} "
          f"({platform.machine()}{', shard %d/%d' % args.shard if args.shard else ''})")
    for suite, reason in not_here:
        print(f"  {suite}: {reason}")
    first = not args.shard or args.shard[0] == 1
    for name, serves, command, cwd in steps(where, suites, gate, first):
        detail = show(command) if command else "the build's test binaries, each test by name"
        print(f"  {name:<18} [{', '.join(serves)}] {detail}")


def build(command, env):
    """Runs the test build and returns {executable: binary name}."""
    done = subprocess.run(command, cwd=LATCH, env=env, stdout=subprocess.PIPE, text=True)
    executables = {}
    for line in done.stdout.splitlines():
        message = json.loads(line)
        if (message.get("reason") == "compiler-artifact" and message["profile"]["test"]
                and message.get("executable")):
            executables[message["executable"]] = message["target"]["name"]
    if done.returncode == 0 and not executables:
        raise SystemExit("test.py: the build made no test binaries")
    return done.returncode, executables


def inventory(executable):
    listing = subprocess.check_output([executable, "--list", "--format", "terse"], text=True)
    return [line.removesuffix(": test") for line in listing.splitlines() if line.endswith(": test")]


def in_shard(binary, name, shard):
    if not shard:
        return True
    index, count = shard
    digest = hashlib.sha256(f"{binary}::{name}".encode()).digest()
    return int.from_bytes(digest[:8], "big") % count == index - 1


def select(where, suites, executables, shard):
    """Each binary's tests for these suites, and how many each suite has."""
    chosen, counts = {}, {suite: 0 for suite in suites if suite != "installer"}
    for executable, binary in sorted(executables.items(), key=lambda item: item[1]):
        names = []
        for name in inventory(executable):
            suite = suite_of(binary, name)
            if suite not in suites:
                continue
            if where == "macos" and suite == "browser" and not runs_on_macos(name):
                continue
            counts[suite] += 1
            if in_shard(binary, name, shard):
                names.append(name)
        chosen[executable] = (binary, names)
    return chosen, counts


def run(args):
    where, suites, gate, not_here = plan(args)
    if args.dry_run:
        print_plan(where, suites, gate, not_here, args)
        return 0
    if where == "container" and not os.environ.get("LATCH_TEST_CONTAINER"):
        return in_container(args)
    print_plan(where, suites, gate, not_here, args)
    env = cargo_env(where)
    first = not args.shard or args.shard[0] == 1
    results = []
    executables = {}
    for name, serves, command, cwd in steps(where, suites, gate, first):
        print(f"\n== {name} [{', '.join(serves)}]", flush=True)
        began = time.monotonic()
        binaries = []
        if name == "build":
            status, executables = build(command, env)
        elif name == "rust":
            status, binaries = run_rust(where, suites, executables, args, env)
        else:
            status = subprocess.run(command, cwd=cwd, env=env).returncode
        results.append((name, status, time.monotonic() - began, None))
        results.extend(binaries)
        if status and name in ("build", "engines"):
            break
    print("\n== summary")
    for name, status, seconds, detail in results:
        print(f"  {'ok  ' if status == 0 else 'FAIL'} {name:<18} {seconds:6.1f}s{'  ' + detail if detail else ''}")
    for suite, reason in not_here:
        print(f"  n/a  {suite:<18} {reason}")
    failed = [name for name, status, _, _ in results if status]
    if args.ci:
        report = {"where": where, "suites": suites, "shard": args.shard,
                  "steps": [{"name": n, "exit": s, "seconds": round(t, 2), "detail": d}
                            for n, s, t, d in results]}
        (DEV / "ci").mkdir(parents=True, exist_ok=True)
        name = f"tests-{where}-{args.shard[0] if args.shard else 1}.json"
        (DEV / "ci" / name).write_text(json.dumps(report, indent=2) + "\n")
    print(f"\ntest.py: {'failed: ' + ', '.join(failed) if failed else 'all passed'}")
    return 1 if failed else 0


def run_rust(where, suites, executables, args, env):
    """Runs each binary once, on its tests in these suites; returns the
    status and a summary row per binary."""
    chosen, counts = select(where, suites, executables, args.shard)
    empty = [suite for suite, count in counts.items() if count == 0]
    if empty:
        print(f"test.py: no Rust tests in {', '.join(empty)} on {where}; the suite is broken")
        return 1, []
    print("  " + ", ".join(f"{suite} {count}" for suite, count in counts.items()), flush=True)
    status, rows = 0, []
    for executable, (binary, names) in chosen.items():
        if not names:
            continue  # libtest would run everything for an empty filter
        print(f"\n-- {binary}: {len(names)} tests", flush=True)
        began = time.monotonic()
        code = subprocess.run([executable, "--exact", *names], cwd=LATCH, env=env).returncode
        rows.append((f"  {binary}", code, time.monotonic() - began, f"{len(names)} tests"))
        status = status or code
    return status, rows


def container_seccomp():
    """latch's container seccomp profile (docker/seccomp.json) with what
    Chrome's own sandbox needs inside its user namespace, in a container
    with no capabilities: chroot (else it aborts, `sys_chroot(...) == 0`),
    and clone with new PID, network, IPC or mount namespaces but no new user
    namespace (else its zygote cannot fork and nothing answers
    `Page.enable`). The kernel still refuses both outside a user namespace.
    Never --no-sandbox."""
    profile = json.loads((LATCH / "docker/seccomp.json").read_text())
    for rule in profile["syscalls"]:
        if rule.get("names") == ["chroot"]:
            rule.pop("includes", None)
    # CLONE_NEWUSER | CLONE_NEWUTS | CLONE_NEWCGROUP all clear.
    profile["syscalls"].append({"names": ["clone"], "action": "SCMP_ACT_ALLOW", "args": [
        {"index": 0, "value": 0x16000000, "valueTwo": 0, "op": "SCMP_CMP_MASKED_EQ"}],
        "includes": {"arches": ["amd64", "arm64"]}})
    path = DEV / "seccomp-test.json"
    DEV.mkdir(exist_ok=True)
    path.write_text(json.dumps(profile))
    return path


def in_container(args):
    """--linux: this same plan, in the Linux test container."""
    if not shutil.which("docker"):
        raise SystemExit("test.py: --linux needs Docker")
    docker = LATCH / "docker"
    subprocess.run(["docker", "build", "--platform", "linux/arm64", "-q", "-t", IMAGE, "-f",
                    docker / "Dockerfile.test", docker], check=True, stdout=subprocess.DEVNULL)
    inner = ["python3", "scripts/test.py", *args.suites]
    if args.shard:
        inner += ["--shard", "%d/%d" % args.shard]
    if args.ci:
        inner.append("--ci")
    # --init: a PID 1 that reaps orphans, as a host's init does; the tests
    # check that a dead host's worker is gone, not left a zombie.
    # On an AppArmor host, docker-default refuses bwrap's mounts; the
    # container runs under latch's own profile, which must be loaded first.
    apparmor = []
    if Path("/sys/module/apparmor/parameters/enabled").exists() and \
            Path("/sys/module/apparmor/parameters/enabled").read_text().strip() == "Y":
        apparmor = ["--security-opt=apparmor=latch-bwrap"]
        print("test.py: AppArmor host: the container runs under latch-bwrap "
              "(sudo apparmor_parser -r latch/docker/latch-bwrap.apparmor)", flush=True)
    command = ["docker", "run", "--rm", "--init", "--platform", "linux/arm64", *apparmor, "--user", f"{os.getuid()}:{os.getgid()}",
               "--cap-drop=ALL", "--security-opt=no-new-privileges=true",
               "--security-opt=systempaths=unconfined",
               f"--security-opt=seccomp={container_seccomp()}",
               "--shm-size=1g", "-v", f"{ROOT}:/repo", "-w", "/repo",
               "-e", "HOME=/tmp", "-e", "LATCH_TEST_CONTAINER=1", "-e", "RUSTUP_HOME=/usr/local/rustup",
               "-e", "CARGO_HOME=/repo/latch/target/cargo-home",
               IMAGE, *inner]
    return subprocess.run(command).returncode


def list_suites():
    width = max(map(len, SUITES))
    print("Suites (`just test SUITE...`; no suite runs `gate`):\n")
    for suite, (what, needs, places) in SUITES.items():
        print(f"  {suite:<{width}}  {what}")
        runs = [label for place, label in (("macos", "macOS"), ("linux", "Linux"), ("container", "--linux"))
                if place in places]
        gates = [label for place, label in (("macos", "macOS"), ("linux", "Linux"), ("container", "--linux"))
                 if suite in GATE[place]]
        print(f"  {'':<{width}}  needs {needs}; runs on {', '.join(runs)}; "
              f"in the gate on {', '.join(gates) or 'none'}")
    print(f"\n  {'gate':<{width}}  everything a merge needs, on this OS: "
          f"macOS {' '.join(GATE['macos'])}; Linux {' '.join(GATE['linux'])}")
    print("\nWhere: this machine, or --linux for the Linux test container (Docker). Not tests: "
          "`just gatekeeper ...` (the real decision model).")


def main():
    sys.stdout.reconfigure(line_buffering=True)
    parser = argparse.ArgumentParser(prog="just test", description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("suites", nargs="*", metavar="SUITE", help="gate (default), " + ", ".join(SUITES))
    parser.add_argument("--linux", action="store_true", help="run in the Linux test container")
    parser.add_argument("--shard", metavar="N/COUNT", help="only this share of the Rust tests (CI)")
    parser.add_argument("--ci", action="store_true", help="also write a report to .dev/ci/")
    parser.add_argument("--dry-run", action="store_true", help="print the plan and run nothing")
    parser.add_argument("--list", action="store_true", help="list the suites")
    args = parser.parse_args()
    if args.list:
        list_suites()
        return 0
    return run(args)


if __name__ == "__main__":
    sys.exit(main())

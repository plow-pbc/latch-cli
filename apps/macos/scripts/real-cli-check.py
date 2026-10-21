"""The app on a real latch against the local Plow stack (API + LinQ twin), with
isolated state under the repo's .dev/: both onboarding runs, the main window
on a live host, the menu bar following a terminal serve, the stdin lifeline,
then sharing-check.sh and measure.sh on the same latch.

    python3 -I scripts/real-cli-check.py --latch PATH [--api URL] [--twin URL] [OUT_DIR]

PATH is a latch built with LATCH_BUILD_API_URL set to the stack's API, so the
app's `latch login --events` signs in there. Drives the test app through its
test hooks, never through UI scripting. Prints no token; the sign-in code
appears only in reports under .dev/.
"""

import argparse
import json
import os
import random
import shutil
import subprocess
import sys
import time
import urllib.request

APP_DIR = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
REPO = os.path.abspath(os.path.join(APP_DIR, "../.."))
DEV = os.path.join(REPO, ".dev/mac-app/real")
TEST_ID = "co.plow.latch.test"
LABEL = TEST_ID + ".serve"

parser = argparse.ArgumentParser()
parser.add_argument("--latch", required=True)
parser.add_argument("--api", default="http://127.0.0.1:19244")
parser.add_argument("--twin", default="http://127.0.0.1:19245")
parser.add_argument("out", nargs="?", default=os.path.join(REPO, ".dev/evidence/mac-app/real-cli"))
args = parser.parse_args()
os.makedirs(args.out, exist_ok=True)
transcript = open(os.path.join(args.out, "real-cli-check.txt"), "w")


def say(text=""):
    print(text, flush=True)
    transcript.write(text + "\n")
    transcript.flush()


def run(*command, **kw):
    return subprocess.run(command, capture_output=True, text=True, **kw)


HOST, HOST2, CLIENT, SHARE = (os.path.join(DEV, d) for d in ("host", "host-not-now", "client", "share"))
shutil.rmtree(DEV, ignore_errors=True)
for d in (HOST, HOST2, CLIENT, SHARE):
    os.makedirs(d)

build = run(os.path.join(APP_DIR, "scripts/build-app.sh"), "--test-hooks", "--latch", args.latch,
            "--serve-state-dir", HOST, cwd=APP_DIR)
if build.returncode != 0:
    sys.exit(build.stderr)
BUNDLE = os.path.join(APP_DIR, build.stdout.strip().splitlines()[-1])
APP = os.path.join(BUNDLE, "Contents/MacOS/PlowLatch")
CLI = os.path.join(BUNDLE, "Contents/Helpers/latch")
TOOL = os.path.join(DEV, "measure-tool")
subprocess.run(["swiftc", "-O", "-o", TOOL, os.path.join(APP_DIR, "scripts/measure.swift")], check=True)

pid = None


def latch(state, *command):
    out = run(CLI, "--state-dir", state, *command)
    return out


def status(state):
    return json.loads(latch(state, "status", "--local").stdout)


def post(command, obj=None):
    subprocess.run([TOOL, "post", command] + ([obj] if obj else []), check=True)


def report():
    path = os.path.join(DEV, "report.json")
    if os.path.exists(path):
        os.remove(path)
    post("report", path)
    for _ in range(40):
        if os.path.exists(path) and os.path.getsize(path):
            return json.load(open(path))
        time.sleep(0.05)
    return {}


def wait_for(test, timeout=30, every=0.25):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        r = report()
        if test(r):
            return r
        time.sleep(every)
    return None


def shown(r):
    """A report without the sign-in code, for the transcript."""
    r = json.loads(json.dumps(r))
    for key in ("code", "smsto", "smsLink", "sendTo"):
        if r.get("onboarding") and key in r["onboarding"]:
            r["onboarding"][key] = "…"
    for key in ("active", "key", "policy", "bundle"):
        r.pop(key, None)
    return json.dumps(r, sort_keys=True)


def launch(state, *extra):
    global pid
    out = run(TOOL, "launch-ms", APP, "-LatchStateDir", state, "-ApplePersistenceIgnoreState", "YES", *extra)
    ms, pid = out.stdout.split()
    pid = int(pid)
    time.sleep(1.5)
    return float(ms)


def quit_app():
    global pid
    if pid is None:
        return
    post("quit")
    for _ in range(50):
        if run("kill", "-0", str(pid)).returncode != 0:
            break
        time.sleep(0.1)
    pid = None


def clear_prefs():
    run("defaults", "delete", TEST_ID)
    for path in (f"~/Library/Preferences/{TEST_ID}.plist", f"~/Library/Saved Application State/{TEST_ID}.savedState"):
        shutil.rmtree(os.path.expanduser(path), ignore_errors=True) if path.endswith("savedState") else \
            os.path.exists(os.path.expanduser(path)) and os.remove(os.path.expanduser(path))


def agent_loaded():
    return run("launchctl", "print", f"gui/{os.getuid()}/{LABEL}").returncode == 0


def snapshot(name):
    path = os.path.join(args.out, name)
    time.sleep(1.2)  # let screen changes and their animations finish
    post("snapshot", path)
    time.sleep(1)
    say(f"  snapshot: {name} ({'saved' if os.path.exists(path) else 'MISSING'})")


def text_code(code, send_to, phone):
    body = json.dumps({"to_phone": send_to, "remote_phone": phone, "text": code}).encode()
    request = urllib.request.Request(f"{args.twin}/ui/inbound", body, {"Content-Type": "application/json"})
    urllib.request.urlopen(request).read()


def onboard(state, start_sharing, name):
    """One full onboarding, from launch to the main window."""
    clear_prefs()
    launch(state)
    say(f"launched on a fresh state: {shown(report())}")
    snapshot(f"{name}-1-welcome.png")
    post("onboarding-next")  # Get started
    r = wait_for(lambda r: (r.get("onboarding") or {}).get("signIn") == "waiting")
    if not r:
        say(f"FAIL: no code: {shown(report())}")
        return False
    code = r["onboarding"]
    expected = f"SMSTO:{code['sendTo']}:{code['code']}"
    link = "sms:" + code["sendTo"] + "?&body=" + code["code"]
    say(f"code shown; QR payload is SMSTO:<number>:<code>: {code['smsto'] == expected}; "
        f"sms link is sms:<number>?&body=<code>: {code['smsLink'] == link}")
    snapshot(f"{name}-2-sign-in.png")
    phone = f"+1555{random.randrange(10**7):07d}"
    say(f"texting the bare code from {phone} through the LinQ twin")
    text_code(code["code"], code["sendTo"], phone)
    r = wait_for(lambda r: (r.get("onboarding") or {}).get("step") == "linked", timeout=30)
    if not r:
        say(f"FAIL: not linked: {shown(report())}")
        return False
    say(f"linked: {shown(r)}")
    snapshot(f"{name}-3-linked.png")
    post("onboarding-next")  # Continue
    post("onboarding-folder", SHARE)
    wait_for(lambda r: (r.get("onboarding") or {}).get("step") == "keepRunning", timeout=5)
    snapshot(f"{name}-4-keep-running.png")
    post("onboarding-keep-running" if start_sharing else "onboarding-not-now")
    r = wait_for(lambda r: (r.get("onboarding") or {}).get("step") == "done", timeout=45)
    if not r:
        say(f"FAIL: never reached Done: {shown(report())}")
        return False
    say(f"done: {shown(r)}")
    snapshot(f"{name}-5-done.png")
    post("onboarding-done")
    r = wait_for(lambda r: r.get("visible") == ["main"] and r.get("onboarding") is None, timeout=5)
    say(f"main window: {shown(r or report())}")
    s = status(state)
    say(f"latch status --local: logged in as {s['peer'] and s['peer']['name']}, serve {json.dumps(s['serve'])}, "
        f"shares {s['shares']}")
    say(f"launchd: {LABEL} {'loaded' if agent_loaded() else 'not loaded'}")
    return True


def cleanup():
    if agent_loaded():
        if pid is None:
            launch(HOST, "-Onboarded", "YES")
        post("stop-sharing")
        wait_for(lambda r: r.get("agent") == "off", timeout=20)
    quit_app()
    clear_prefs()


try:
    say(f"latch: {latch(HOST, '--version').stdout.strip()}")
    say(f"before: launchd {LABEL} {'loaded' if agent_loaded() else 'not loaded'}")

    say("\n== Chunk 3, run 1: Welcome → Sign in → Linked → Keep Latch running → Start Sharing → Done")
    if not onboard(HOST, True, "run1-start-sharing"):
        sys.exit(1)
    say(f"run 1 serving: {status(HOST)['serve']['ready']}")

    say("\n== Chunk 2: the main window on the live host")
    snapshot("ch2-overview.png")
    host_name = status(HOST)["peer"]["name"]
    login = latch(CLIENT, "login", "--api-url", args.api, "--token-file", os.path.join(HOST, "plow-token"),
                  "--name", host_name + "-laptop")
    say(f"a second peer logs in with the owner's login: exit {login.returncode}")
    post("show-screen", "activity")
    time.sleep(1)
    for word in ("one", "two"):
        out = latch(CLIENT, "exec", "--host", host_name, "--", "echo", word)
        say(f"latch exec --host {host_name} -- echo {word}: exit {out.returncode}")
    r = wait_for(lambda r: len([a for a in r.get("activity", []) if a.startswith("exec")]) >= 2, timeout=15)
    say(f"Activity on screen (newest first, connects hidden): {(r or report()).get('activity')}")
    snapshot("ch2-activity.png")

    say("\n== Chunk 2: the menu bar follows a serve started and stopped in a terminal, windows closed")
    post("stop-sharing")
    wait_for(lambda r: r.get("agent") == "off" and r.get("state") == "Sharing stopped", timeout=30)
    post("close-windows")
    time.sleep(1)
    terminal = subprocess.Popen([CLI, "--state-dir", HOST, "serve", "--no-dashboard"],
                                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    # Timed from when latch itself says the host serves (or stopped), to
    # when the menu bar shows it.
    deadline = time.monotonic() + 60
    while not status(HOST)["serve"]["ready"] and time.monotonic() < deadline:
        time.sleep(0.05)
    started = time.monotonic()
    r = wait_for(lambda r: r.get("menu") == "link.circle.fill", timeout=10, every=0.05)
    took = time.monotonic() - started
    say(f"terminal serve started: menu shows {r and r['state']!r} {took:.2f} s after latch says serving "
        f"(budget 2 s): {'PASS' if r and took <= 2 else 'FAIL'}")
    terminal.terminate()
    terminal.wait(timeout=10)
    started = time.monotonic()
    r = wait_for(lambda r: r.get("menu") == "link.circle", timeout=10, every=0.05)
    took = time.monotonic() - started
    say(f"terminal serve stopped: menu shows {r and r['state']!r} {took:.2f} s after it exited "
        f"(budget 2 s): {'PASS' if r and took <= 2 else 'FAIL'}")

    say("\n== Chunk 2 #1: the stdin lifeline — kill -9 the app, its latch streams exit")
    post("show-main")
    post("show-screen", "activity")
    time.sleep(2)
    streams = run("pgrep", "-f", f"{CLI} --state-dir {HOST} events").stdout.split()
    say(f"latch events processes while the app runs: {len(streams)}")
    run("kill", "-9", str(pid))
    pid = None
    time.sleep(2)
    left = [p for p in streams if run("kill", "-0", p).returncode == 0]
    say(f"2 s after kill -9: {len(left)} left: {'PASS' if streams and not left else 'FAIL'}")

    say("\n== Chunk 3, run 2: … → Keep Latch running → Not now → Done")
    if not onboard(HOST2, False, "run2-not-now"):
        sys.exit(1)
    s = status(HOST2)
    say(f"run 2: folders saved {s['shares'] == [os.path.realpath(SHARE)]}, serving {s['serve']['running']}, "
        f"agent loaded {agent_loaded()}")
    quit_app()
finally:
    cleanup()
    say(f"\nafter: launchd {LABEL} {'loaded' if agent_loaded() else 'not loaded'}")

#!/usr/bin/env python3
"""Logs a state folder in to a local Plow stack, by text, as a person would:
runs `latch login --events` and texts its code to the stack's Linq twin
from a made-up phone. The same phone is the same local account each time.
Never for production Plow; tokens stay in the state folder, never printed.
No credential reaches the local stack but the one its texted code mints:
latch gets none of the environment's, and a state folder logged in to
another Plow is refused.

    dev-login.py LATCH STATE API TWIN [--phone +1555…] [--name NAME]
"""

import argparse
import json
import os
import subprocess
import sys
import urllib.request

# The environment latch takes a Plow credential or another Plow from.
CREDENTIAL_ENV = ("PLOW_AGENT_TOKEN", "PLOW_TOKEN_FILE", "PLOW_API_BASE", "PLOW_API_URL")


def saved_api(state):
    """The Plow the state folder is logged in to, if it names one."""
    for name, read in (("config.json", lambda text: json.loads(text)["api_url"]),
                       ("plow-url", str.strip)):
        try:
            with open(os.path.join(state, name)) as file:
                return read(file.read())
        except FileNotFoundError:
            continue
    return None


def child_env():
    """This environment, less every credential; the agent token file latch
    would otherwise look for is one that is never there."""
    env = {k: v for k, v in os.environ.items() if k not in CREDENTIAL_ENV}
    env["PLOW_TOKEN_FILE"] = os.devnull + ".none"
    return env


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("latch")
    parser.add_argument("state")
    parser.add_argument("api")
    parser.add_argument("twin")
    parser.add_argument("--phone", default="+15550100201")
    parser.add_argument("--name", default="dev")
    args = parser.parse_args()
    saved = saved_api(args.state)
    if saved is not None and saved.rstrip("/") != args.api.rstrip("/"):
        print(f"{args.state} is logged in to {saved}, not {args.api}; nothing was sent. "
              "Log it out or use another state folder", file=sys.stderr)
        return 2
    login = subprocess.Popen(
        [args.latch, "--state-dir", args.state, "login", "--events",
         "--api-url", args.api, "--name", args.name],
        stdout=subprocess.PIPE, text=True, env=child_env(),
    )
    for line in login.stdout:
        event = json.loads(line)
        if event["type"] == "code":
            text = f"Plow Activate: {event['code']}"
            body = {"remote_phone": args.phone, "to_phone": event["send_to"], "text": text}
            request = urllib.request.Request(
                f"{args.twin}/ui/inbound",
                data=json.dumps(body).encode(),
                headers={"content-type": "application/json"},
            )
            urllib.request.urlopen(request, timeout=10).close()
        elif event["type"] == "verified":
            print(f"{args.state} is logged in to {args.api} as {event['peer']['name']}")
        elif event["type"] in ("error", "expired"):
            print(f"login failed: {event.get('error', event['type'])}", file=sys.stderr)
    return login.wait()


if __name__ == "__main__":
    sys.exit(main())

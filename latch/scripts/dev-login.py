#!/usr/bin/env python3
"""Logs a state folder in to a local Plow stack, by text, as a person would:
runs `latch login --events` and texts its code to the stack's Linq twin
from a made-up phone. The same phone is the same local account each time.
Never for production Plow; tokens stay in the state folder, never printed.

    dev-login.py LATCH STATE API TWIN [--phone +1555…] [--name NAME]
"""

import argparse
import json
import subprocess
import sys
import urllib.request


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("latch")
    parser.add_argument("state")
    parser.add_argument("api")
    parser.add_argument("twin")
    parser.add_argument("--phone", default="+15550100201")
    parser.add_argument("--name", default="dev")
    args = parser.parse_args()
    login = subprocess.Popen(
        [args.latch, "--state-dir", args.state, "login", "--events",
         "--api-url", args.api, "--name", args.name],
        stdout=subprocess.PIPE, text=True,
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

#!/usr/bin/env python3
"""The local Plow stack the dev recipes use: its API and its Linq twin.
Each is, first found: LATCH_DEV_PLOW_API / LATCH_DEV_PLOW_TWIN; the
.plow-dev-env of the plow checkout LATCH_DEV_PLOW_CHECKOUT names; the plow
checkout "main"'s ports. Read from the environment (`just` loads the repo's
.env.local), never by latch itself.

    dev_plow.py api    prints the API's URL
"""

import os
from pathlib import Path
import sys
import urllib.error
import urllib.request

API, TWIN, CHECKOUT = "LATCH_DEV_PLOW_API", "LATCH_DEV_PLOW_TWIN", "LATCH_DEV_PLOW_CHECKOUT"
DEFAULTS = {API: "http://127.0.0.1:18934", TWIN: "http://127.0.0.1:18935"}
HOW = (f"Start plow's dev stack (`just api up` and `just dtu linq up` in a plow checkout), "
       f"then point latch at it with {CHECKOUT}, or {API} and {TWIN}, in .env.local "
       f"(see .env.local.example)")


class Unset(Exception):
    pass


def checkout_env(checkout):
    """The KEY=VALUE lines of a plow checkout's .plow-dev-env."""
    path = Path(checkout).expanduser() / ".plow-dev-env"
    try:
        lines = path.read_text().splitlines()
    except OSError as e:
        raise Unset(f"{CHECKOUT}={checkout} has no readable .plow-dev-env ({e.strerror}): "
                    f"run `just api up` in that checkout, or set {API} and {TWIN}")
    return dict(line.split("=", 1) for line in lines if "=" in line and not line.startswith("#"))


def address(name, env=os.environ):
    """The URL for `name` (API or TWIN), and where it came from."""
    if env.get(name):
        return env[name], name
    if env.get(CHECKOUT):
        plow = checkout_env(env[CHECKOUT])
        if name == API and plow.get("PLOW_API_URL"):
            return plow["PLOW_API_URL"], f"PLOW_API_URL in {CHECKOUT}'s .plow-dev-env"
        if name == TWIN and plow.get("PLOW_DTU_LINQ_PORT"):
            return (f"http://127.0.0.1:{plow['PLOW_DTU_LINQ_PORT']}",
                    f"PLOW_DTU_LINQ_PORT in {CHECKOUT}'s .plow-dev-env")
        raise Unset(f"{CHECKOUT}={env[CHECKOUT]}'s .plow-dev-env does not say where its "
                    f"{'API' if name == API else 'Linq twin'} is: set {name}")
    return DEFAULTS[name], "the default"


def unreachable(what, url, source, path):
    """Why `url` cannot be reached, or None. Any HTTP answer is reachable."""
    try:
        urllib.request.urlopen(url.rstrip("/") + path, timeout=3).close()
    except urllib.error.HTTPError:
        pass
    except (urllib.error.URLError, OSError) as e:
        why = getattr(e, "reason", e)
        return f"the local Plow's {what} at {url} ({source}) isn't reachable: {why}. {HOW}"
    return None


if __name__ == "__main__":
    if sys.argv[1:] != ["api"]:
        sys.exit("usage: dev_plow.py api")
    try:
        print(address(API)[0])
    except Unset as e:
        sys.exit(str(e))

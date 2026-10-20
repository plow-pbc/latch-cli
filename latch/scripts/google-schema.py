"""Snapshot the reviewed grammar from `gog schema --json` for the pinned binary."""

import json
import subprocess
import sys
from pathlib import Path

allowed = {
    "gmail.search",
    "gmail.messages.search",
    "gmail.get",
    "gmail.thread.get",
    "gmail.labels.list",
    "gmail.drafts.list",
    "gmail.drafts.get",
    "gmail.attachment",
    "gmail.send",
    "gmail.reply",
    "gmail.reply-all",
    "gmail.forward",
    "gmail.drafts.create",
    "gmail.drafts.update",
    "gmail.drafts.reply",
    "gmail.drafts.reply-all",
    "gmail.drafts.forward",
    "gmail.drafts.send",
    "gmail.drafts.delete",
    "gmail.archive",
    "gmail.mark-read",
    "gmail.unread",
    "gmail.messages.modify",
    "calendar.calendars",
    "calendar.events",
    "calendar.event",
    "calendar.search",
    "calendar.freebusy",
    "calendar.conflicts",
    "calendar.create",
    "calendar.update",
    "calendar.move",
    "calendar.delete",
    "calendar.respond",
}

schema = json.loads(subprocess.check_output([sys.argv[1], "schema", "--json"]))[
    "command"
]
roots = {f["name"] for f in schema["flags"]}
kept = {"help", "select", "force"}


def node(n, path):
    command = ".".join(path)
    children = [
        x
        for c in n.get("subcommands", [])
        if (x := node(c, path + [c["name"]])) is not None
    ]
    if command not in allowed and not children:
        return None
    retained = {child["name"] for child in children}
    excluded = [
        name
        for child in n.get("subcommands", [])
        if child["name"] not in retained
        for name in [child["name"], *child.get("aliases", [])]
    ]
    flags = []
    for f in n.get("flags", []):
        if f["name"] in roots and f["name"] not in kept:
            continue
        if (
            f["name"] in ("inline", "inline-max-bytes", "track", "track-split", "raw-file")
            or (command == "gmail.archive" and f["name"] == "thread")
            or (
                command in ("gmail.archive", "gmail.mark-read", "gmail.unread")
                and f["name"] == "query"
            )
        ):
            continue
        flags.append(
            {
                "name": f["name"],
                "aliases": f.get("aliases", [])
                + ([f["short"]] if f.get("short") else []),
                "value": f["type"] not in ("bool", "*bool", "kong.helpFlag"),
            }
        )
    return {
        "name": n["name"],
        "aliases": n.get("aliases", []),
        "command": command if command in allowed else None,
        "default": n.get("default_cmd"),
        "flags": flags,
        "children": children,
        "excluded": excluded,
    }


Path(sys.argv[2]).write_text(json.dumps(node(schema, []), separators=(",", ":")) + "\n")

#!/usr/bin/env python3
"""The plan command's metrics, read from a host's audit log.

    scripts/plan-metrics.py STATE_DIR/audit.ndjson [--since ISO-TIME]

Prints one JSON object: each plan request (actions, model-chosen actions,
model calls, the model's total time and the cost, confirmations, the stop),
and over all of them the plan share, confirmations and how they were
answered, what followed each stop, escapes to `eval`, agent turns and
actions per turn, stops by code, and model calls and time per plan and per
action. Turns are the caller's `--turn` / `LATCH_TURN`, else its parent pid.

Cost is an estimate: the decision route reports no price per call, so it
is the call count times PRICE_PER_CALL, Plow's stated price for one call.
"""

import argparse
import json
import sys
from collections import Counter, defaultdict

# Plow's decision route: "about $0.0001 a call" (its own notes).
PRICE_PER_CALL = 0.0001


def browser_records(path, since):
    with open(path) as log:
        for line in log:
            try:
                record = json.loads(line)
            except json.JSONDecodeError:
                continue
            if record.get("op") != "browser" or (since and record["time"] < since):
                continue
            yield record


def turn_of(args):
    if args.get("turn"):
        return str(args["turn"])
    if args.get("ppid") is not None:
        return f"ppid:{args['ppid']}"
    return "unknown"


def plan_of(record):
    args = record["args"] or {}
    actions = [r for r in args.get("step_records", []) if "action" in r]
    calls = args.get("model_calls", [])
    stop = args.get("stop") or {}
    return {
        "time": record["time"],
        "request": args.get("request"),
        "session": args.get("session"),
        "turn": turn_of(args),
        "status": args.get("status", record.get("outcome")),
        "stop": stop.get("code"),
        "stop_step": stop.get("step"),
        "steps": args.get("steps"),
        "goals": args.get("goals", 0),
        "step_outcomes": args.get("step_outcomes", []),
        "actions": len(actions),
        "model_actions": sum(1 for r in actions if r.get("chosen") == "model"),
        "left_out": sum(1 for r in actions if r.get("left_out")),
        "confirmations_issued": sum(
            1 for r in args.get("step_records", []) if r.get("confirmation") and r.get("dispatch") == "refused"
        ),
        "confirmation_used": args.get("confirmation"),
        "model_calls": len(calls),
        "model_calls_by_purpose": dict(Counter(c.get("purpose") for c in calls)),
        "model_ms": sum(c.get("latency_ms", 0) for c in calls),
        "model_failed": sum(1 for c in calls if c.get("outcome") != "ok"),
        "cost_usd_estimate": round(len(calls) * PRICE_PER_CALL, 6),
    }


def metrics(records):
    plans = []
    # Each session's commands in order, for what follows a stop.
    by_session = defaultdict(list)
    escapes = 0
    for record in records:
        args = record["args"] or {}
        command = args.get("command")
        if command == "do" and args.get("request"):
            plan = plan_of(record)
            plans.append(plan)
            by_session[plan["session"]].append(("do", plan))
        elif command == "eval":
            escapes += 1
            by_session[args.get("session")].append(("eval", None))

    continuations = Counter()
    for commands in by_session.values():
        for (kind, plan), (next_kind, following) in zip(commands, commands[1:]):
            if kind != "do" or plan["status"] != "stopped":
                continue
            if next_kind == "eval":
                continuations["eval"] += 1
            elif following["confirmation_used"]:
                continuations["confirmation"] += 1
            elif following["goals"]:
                continuations["new_plan"] += 1
            else:
                continuations["literal_repair"] += 1

    answered = Counter()
    for plan in plans:
        if plan["confirmation_used"]:
            answered["confirm_stale" if plan["stop"] == "confirm_stale" else "carried_out"] += 1

    actions = sum(p["actions"] for p in plans)
    model_actions = sum(p["model_actions"] for p in plans)
    calls = sum(p["model_calls"] for p in plans)
    model_ms = sum(p["model_ms"] for p in plans)
    turns = Counter(p["turn"] for p in plans)
    turn_actions = Counter()
    for plan in plans:
        turn_actions[plan["turn"]] += plan["actions"]
    return {
        "price_per_call_usd": PRICE_PER_CALL,
        "cost_is_estimate": True,
        "plans": plans,
        "totals": {
            "plans": len(plans),
            "actions": actions,
            "model_actions": model_actions,
            "plan_share": round(model_actions / actions, 3) if actions else None,
            "confirmations_issued": sum(p["confirmations_issued"] for p in plans),
            "confirmations_answered": dict(answered),
            "continuations": dict(continuations),
            "escapes_to_eval": escapes,
            "agent_turns": len(turns),
            "actions_per_turn": round(actions / len(turns), 2) if turns else None,
            "stops_by_code": dict(Counter(p["stop"] for p in plans if p["stop"])),
            "model_calls": calls,
            "model_calls_per_plan": round(calls / len(plans), 2) if plans else None,
            "model_calls_per_action": round(calls / actions, 2) if actions else None,
            "model_ms": model_ms,
            "model_ms_per_plan": round(model_ms / len(plans)) if plans else None,
            "model_ms_per_action": round(model_ms / actions) if actions else None,
            "cost_usd_estimate": round(calls * PRICE_PER_CALL, 6),
        },
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("audit", help="the host's audit.ndjson")
    parser.add_argument("--since", help="only records from this ISO time on")
    args = parser.parse_args()
    json.dump(metrics(browser_records(args.audit, args.since)), sys.stdout, indent=2)
    print()


if __name__ == "__main__":
    main()

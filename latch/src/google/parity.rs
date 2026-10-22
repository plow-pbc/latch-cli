//! The same argv through latch and through plow-gog, the planner in the old
//! Latch app (`packages/device-core/src/providers/plowGog.ts`) that Plow's
//! Google route runs too. Each row is one of plow-gog's own test rows
//! (`test/plowGog.test.ts`), with what plow-gog plans and what latch plans
//! for it. Where latch deliberately differs, the row says why: latch keeps
//! its own rules where it is stricter (the exact command list and flags),
//! and keeps its `{items, accounts}` envelope for a fan-out read even when
//! one account is named.

use serde_json::{Value, json};

use super::{Request, host};

const PACIFIC: &str = "America/Los_Angeles";

/// What latch makes of `argv` after `latch google --`, before any account is
/// looked up: the parts plow-gog's plan also has.
fn latch_plan(argv: &[&str]) -> Value {
    // The client answers these itself, with the list of allowed commands.
    if matches!(argv, ["help" | "--help" | "-h"]) {
        return json!({"kind": "help"});
    }
    let mut request = Request {
        gog_args: argv.iter().map(|arg| arg.to_string()).collect(),
        account: vec![],
        confirm_conflict: false,
        inputs: vec![],
    };
    match host::plan(&mut request, PACIFIC) {
        Err(failure) => json!({"kind": "refused", "code": failure.code}),
        Ok(None) => json!({"kind": "accounts"}),
        Ok(Some(plan)) if plan.help => json!({"kind": "help"}),
        Ok(Some(plan)) => json!({
            "kind": if plan.fanout() { "fanout" } else { "single" },
            "command": plan.command,
            "accounts": request.account,
            "confirm_conflict": request.confirm_conflict,
            "max": plan.flag("max"),
            "timezone": plan.flag("timezone"),
            "send_updates": plan.flag("send-updates"),
            "compact": plan.fanout() && plan.compact(),
            "conflict_check": plan.conflict_check(),
        }),
    }
}

/// A plan with every field at its default, then `fields`.
fn planned(kind: &str, command: &str, fields: Value) -> Value {
    let mut plan = json!({
        "kind": kind,
        "command": command,
        "accounts": [],
        "confirm_conflict": false,
        "max": null,
        "timezone": null,
        "send_updates": null,
        "compact": false,
        "conflict_check": null,
    });
    for (key, value) in fields.as_object().expect("fields") {
        plan[key] = value.clone();
    }
    plan
}
fn fanout(command: &str, fields: Value) -> Value {
    planned("fanout", command, fields)
}
fn single(command: &str, fields: Value) -> Value {
    planned("single", command, fields)
}
fn refused(code: &str) -> Value {
    json!({"kind": "refused", "code": code})
}

struct Row {
    /// plow-gog's test name for the row.
    why: &'static str,
    argv: &'static [&'static str],
    latch: Value,
    /// Why latch's plan differs from plow-gog's, when it does.
    differs: Option<&'static str>,
}

fn rows() -> Vec<Row> {
    let a = "a@example.com";
    let b = "b@example.com";
    let calendar_events = |fields: Value| {
        let mut plan = fanout(
            "calendar.events",
            json!({"max": "100", "timezone": PACIFIC, "compact": true}),
        );
        for (key, value) in fields.as_object().expect("fields") {
            plan[key] = value.clone();
        }
        plan
    };
    let create = |fields: Value| {
        let mut plan = single(
            "calendar.create",
            json!({"send_updates": "all", "conflict_check": "primary"}),
        );
        for (key, value) in fields.as_object().expect("fields") {
            plan[key] = value.clone();
        }
        plan
    };
    vec![
        Row {
            why: "fans a gmail search out across accounts, sorted by date",
            argv: &["gmail", "search", "newer_than:7d"],
            latch: fanout("gmail.search", json!({})),
            differs: None,
        },
        Row {
            why: "leaves a stray --calendars on a gmail fan-out to gog",
            argv: &["gmail", "search", "q", "--calendars=a"],
            latch: refused("not_allowed"),
            differs: Some(
                "latch reads every flag against gog's grammar; gmail search has no --calendars",
            ),
        },
        Row {
            why: "narrows a fan-out to the several accounts --account names",
            argv: &[
                "gmail",
                "search",
                "q",
                "--account",
                "a@example.com,b@example.com",
            ],
            latch: fanout("gmail.search", json!({"accounts": [a, b]})),
            differs: None,
        },
        Row {
            why: "reads gog's -a shorthand as --account, value in the next argument",
            argv: &["gmail", "search", "q", "-a", "a@example.com,b@example.com"],
            latch: fanout("gmail.search", json!({"accounts": [a, b]})),
            differs: None,
        },
        Row {
            why: "reads the joined -a=<v> shorthand",
            argv: &["gmail", "get", "m1", "-a=b@example.com"],
            latch: single("gmail.get", json!({"accounts": [b]})),
            differs: None,
        },
        Row {
            why: "reads the attached -a<v> shorthand",
            argv: &["gmail", "get", "m1", "-ab@example.com"],
            latch: single("gmail.get", json!({"accounts": [b]})),
            differs: None,
        },
        Row {
            why: "leaves a shorthand cluster alone, forwarding it to gog",
            argv: &["gmail", "get", "m1", "-ja", "b@example.com"],
            latch: refused("not_allowed"),
            differs: Some(
                "gog's -j (--json) is a global flag, which latch sets itself and never takes",
            ),
        },
        Row {
            why: "classifies through gog's own aliases but keeps the agent's spelling",
            argv: &["mail", "search", "q"],
            latch: fanout("gmail.search", json!({})),
            differs: None,
        },
        Row {
            why: "fans calendar events out, sorted by start, 100 deep by default, in the owner's zone, compacted",
            argv: &["calendar", "events", "primary"],
            latch: calendar_events(json!({})),
            differs: None,
        },
        Row {
            why: "keeps the agent's own --timezone and --select on a calendar list, uncompacted",
            argv: &[
                "calendar",
                "events",
                "--timezone=UTC",
                "--select",
                "summary,startDayOfWeek",
            ],
            latch: calendar_events(json!({"timezone": "UTC", "compact": false})),
            differs: None,
        },
        Row {
            why: "keeps the agent's own --timezone on a compacted list",
            argv: &["calendar", "events", "--timezone", "Asia/Tokyo"],
            latch: calendar_events(json!({"timezone": "Asia/Tokyo"})),
            differs: None,
        },
        Row {
            why: "keeps the agent's own --max on a calendar list",
            argv: &["cal", "ls", "--max=5"],
            latch: calendar_events(json!({"max": "5"})),
            differs: None,
        },
        Row {
            why: "fans freebusy out with no sort",
            argv: &["cal", "freebusy", "primary"],
            latch: fanout("calendar.freebusy", json!({})),
            differs: None,
        },
        Row {
            why: "fans conflicts out with no sort",
            argv: &["calendar", "conflicts", "--from", "x", "--to", "y"],
            latch: fanout("calendar.conflicts", json!({})),
            differs: None,
        },
        Row {
            why: "does not double a --json the agent already passed",
            argv: &["gmail", "search", "q", "--json"],
            latch: fanout("gmail.search", json!({})),
            differs: None,
        },
        Row {
            why: "narrows a read to one account when --account is given",
            argv: &["gmail", "search", "q", "--account", "a@example.com"],
            latch: fanout("gmail.search", json!({"accounts": [a]})),
            differs: Some(
                "plow-gog runs one named account as a single command; latch keeps the merged envelope",
            ),
        },
        Row {
            why: "reads the joined --account=<v> spelling too",
            argv: &["gmail", "search", "q", "--account=b@example.com"],
            latch: fanout("gmail.search", json!({"accounts": [b]})),
            differs: Some("as above: one named account keeps the merged envelope"),
        },
        Row {
            why: "narrows a --calendars read to the one account that owns them",
            argv: &[
                "calendar",
                "events",
                "list",
                "--calendars=a,b",
                "--account",
                "a@example.com",
            ],
            latch: calendar_events(json!({"accounts": [a]})),
            differs: Some("as above: one named account keeps the merged envelope"),
        },
        Row {
            why: "leaves everything uncurated a single-account command, resolved at run time",
            argv: &["gmail", "get", "msg-1", "--json"],
            latch: single("gmail.get", json!({})),
            differs: None,
        },
        Row {
            why: "leaves a send a single-account command",
            argv: &[
                "gmail",
                "send",
                "--to",
                "x@y.com",
                "--subject",
                "s",
                "--body",
                "b",
            ],
            latch: single("gmail.send", json!({})),
            differs: None,
        },
        Row {
            why: "carries --account on any single",
            argv: &[
                "gmail",
                "drafts",
                "reply",
                "m1",
                "--body",
                "b",
                "--account",
                "b@example.com",
            ],
            latch: single("gmail.drafts.reply", json!({"accounts": [b]})),
            differs: None,
        },
        Row {
            why: "gates a timed calendar create on a conflict check",
            argv: &[
                "calendar",
                "create",
                "primary",
                "--summary",
                "X",
                "--from",
                "2026-08-28T10:00:00-07:00",
                "--to",
                "2026-08-28T11:00:00-07:00",
            ],
            latch: create(json!({})),
            differs: None,
        },
        Row {
            why: "never gates an update, even a timed one",
            argv: &[
                "calendar",
                "update",
                "primary",
                "e1",
                "--from=2026-08-28T10:00:00Z",
                "--to=2026-08-28T11:00:00Z",
            ],
            latch: single("calendar.update", json!({"send_updates": "all"})),
            differs: None,
        },
        Row {
            why: "reads the joined --from=/--to= spelling for the gate",
            argv: &[
                "calendar",
                "create",
                "primary",
                "--from=2026-08-28T10:00:00Z",
                "--to=2026-08-28T11:00:00Z",
            ],
            latch: create(json!({})),
            differs: None,
        },
        Row {
            why: "lets an all-day create (date-only bounds) skip the gate",
            argv: &[
                "calendar",
                "create",
                "primary",
                "--summary",
                "X",
                "--from",
                "2026-08-28",
                "--to",
                "2026-08-29",
            ],
            latch: create(json!({})),
            differs: Some(
                "latch skips the check only for --all-day, which gog needs for a date-only event; any other create is checked",
            ),
        },
        Row {
            why: "(latch) an --all-day create skips the gate",
            argv: &[
                "calendar",
                "create",
                "primary",
                "--all-day",
                "--from",
                "2026-08-28",
                "--to",
                "2026-08-29",
            ],
            latch: create(json!({"conflict_check": null})),
            differs: None,
        },
        Row {
            why: "(latch) the gate checks the calendar being booked on",
            argv: &[
                "cal",
                "add",
                "team@group.calendar.google.com",
                "--from=2026-08-28T10:00:00Z",
                "--to=2026-08-28T11:00:00Z",
            ],
            latch: create(json!({"conflict_check": "team@group.calendar.google.com"})),
            differs: None,
        },
        Row {
            why: "never gates a delete, timed window or not",
            argv: &["calendar", "delete", "primary", "e1"],
            latch: single("calendar.delete", json!({"send_updates": "all"})),
            differs: None,
        },
        Row {
            why: "keeps the agent's own --send-updates on a calendar write",
            argv: &["cal", "rm", "primary", "e1", "--send-updates=none"],
            latch: single("calendar.delete", json!({"send_updates": "none"})),
            differs: None,
        },
        Row {
            why: "notifies on a move too, under gog's alias",
            argv: &["cal", "transfer", "primary", "e1", "other"],
            latch: single("calendar.move", json!({"send_updates": "all"})),
            differs: None,
        },
        Row {
            why: "strips --confirm-conflict and carries it as the override",
            argv: &[
                "calendar",
                "create",
                "primary",
                "--summary",
                "X",
                "--from",
                "2026-08-28T10:00:00Z",
                "--to",
                "2026-08-28T11:00:00Z",
                "--confirm-conflict",
            ],
            latch: create(json!({"confirm_conflict": true})),
            differs: None,
        },
        Row {
            why: "answers the accounts verb without touching gog",
            argv: &["accounts"],
            latch: json!({"kind": "accounts"}),
            differs: None,
        },
        Row {
            why: "passes help through, still spelled by the agent",
            argv: &["gmail", "--help"],
            latch: json!({"kind": "help"}),
            differs: None,
        },
        Row {
            why: "(latch) help for one command",
            argv: &["gmail", "search", "--help"],
            latch: json!({"kind": "help"}),
            differs: None,
        },
        Row {
            why: "passes top-level help through",
            argv: &["--help"],
            latch: json!({"kind": "help"}),
            differs: None,
        },
        // plow-gog's refusals.
        Row {
            why: "refuses a flag that would disarm the belt",
            argv: &["gmail", "search", "q", "--readonly=false"],
            latch: refused("not_allowed"),
            differs: None,
        },
        Row {
            why: "refuses a group outside the token's scopes",
            argv: &["drive", "ls"],
            latch: refused("not_allowed"),
            differs: None,
        },
        Row {
            why: "refuses a leading global flag",
            argv: &["--json", "gmail", "search", "q"],
            latch: refused("not_allowed"),
            differs: None,
        },
        Row {
            why: "refuses the dotted spelling",
            argv: &["gmail.search", "q"],
            latch: refused("not_allowed"),
            differs: None,
        },
        Row {
            why: "refuses arguments after the accounts verb",
            argv: &["accounts", "sneakyagenttext"],
            latch: refused("not_allowed"),
            differs: None,
        },
        Row {
            why: "refuses an --account with no value",
            argv: &["gmail", "search", "q", "--account"],
            latch: refused("usage"),
            differs: None,
        },
        Row {
            why: "refuses a -a with no value",
            argv: &["gmail", "search", "q", "-a"],
            latch: refused("usage"),
            differs: None,
        },
        Row {
            why: "refuses an empty -a= value",
            argv: &["gmail", "search", "q", "-a="],
            latch: refused("usage"),
            differs: None,
        },
        Row {
            why: "refuses --calendars under a fan-out",
            argv: &[
                "calendar",
                "events",
                "list",
                "--calendars=sneakyagenttext",
                "--from=now",
            ],
            latch: refused("account_required"),
            differs: None,
        },
        Row {
            why: "(latch) --calendars under a fan-out of several named accounts is refused too",
            argv: &[
                "calendar",
                "events",
                "--calendars",
                "a,b",
                "--account",
                "a@example.com,b@example.com",
            ],
            latch: refused("account_required"),
            differs: None,
        },
        Row {
            why: "(latch) a nested group's help",
            argv: &["mail", "labels", "--help"],
            latch: json!({"kind": "help"}),
            differs: None,
        },
        Row {
            why: "(latch) -j is --json too",
            argv: &["gmail", "search", "q", "-j"],
            latch: fanout("gmail.search", json!({})),
            differs: None,
        },
        Row {
            why: "(latch) words after a bare -- stay gog's values",
            argv: &["gmail", "search", "--", "-a", "--confirm-conflict"],
            latch: fanout("gmail.search", json!({})),
            differs: None,
        },
    ]
}

#[test]
fn latch_plans_each_argv_as_plow_gog_does_or_says_why_not() {
    let mut failures = Vec::new();
    for row in rows() {
        let got = latch_plan(row.argv);
        if got != row.latch {
            failures.push(format!(
                "{} {:?}\n  expected {}\n  got      {}",
                row.why, row.argv, row.latch, got
            ));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
    // The rows that differ are few, and each says why.
    let differs: Vec<_> = rows()
        .into_iter()
        .filter(|row| row.differs.is_some())
        .collect();
    assert!(differs.iter().all(|row| !row.differs.unwrap().is_empty()));
}

#[test]
fn several_accounts_on_a_one_account_command_are_refused_at_run_time() {
    // plow-gog refuses `gmail get msg-1 --account=a,b` while planning; latch
    // plans it and refuses it when it picks accounts (exit 2).
    let mut request = Request {
        gog_args: [
            "gmail",
            "get",
            "msg-1",
            "--account=a@example.com,b@example.com",
        ]
        .map(str::to_owned)
        .to_vec(),
        account: vec![],
        confirm_conflict: false,
        inputs: vec![],
    };
    let plan = host::plan(&mut request, PACIFIC).unwrap().unwrap();
    assert_eq!(request.account, ["a@example.com", "b@example.com"]);
    let tokens: super::Tokens = serde_json::from_value(json!({
        "accounts": [
            {"account": "a@example.com", "access_token": "test-placeholder", "is_default": true,
             "capabilities": {"mail_read": true, "mail_write": true,
                              "calendar_read": true, "calendar_write": true}},
            {"account": "b@example.com", "access_token": "test-placeholder", "is_default": false,
             "capabilities": {"mail_read": true, "mail_write": true,
                              "calendar_read": true, "calendar_write": true}},
        ],
    }))
    .unwrap();
    let failure = host::select(&tokens, &request.account, plan.fanout())
        .err()
        .expect("one account only");
    assert_eq!(failure.code, "account_required");
}

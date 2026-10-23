use super::*;
use crate::lifecycle::Problem;
use crate::status::{Account, Host, Hosting, Login, ThisMachine};
use serde_json::json;

/// A Mac signed in as the owner, serving in the background, Google and
/// Messages on, Contacts off, checked for updates this morning.
fn facts() -> Facts {
    Facts {
        local: LocalStatus {
            login: Some(Login {
                kind: "device".into(),
            }),
            this_machine: ThisMachine {
                name: Some("studio".into()),
                key: Some("k".into()),
                hosting: Hosting {
                    running: true,
                    ready: true,
                    configuring: false,
                    managed: true,
                    attached: false,
                    state: Phase::Serving,
                    pid: Some(4242),
                    blocked: None,
                    deny_exec: Some(crate::serve::DenyExec::from_flags(&[])),
                },
            },
            plugins: json!([{"name": "google", "state": "enabled"},
                            {"name": "messages", "state": "enabled"},
                            {"name": "contacts", "state": "disabled",
                             "enable": "latch plugins enable contacts"}]),
            update: crate::update::Status {
                available: false,
                build: Some(140),
                checked_at: Some("2026-10-23T15:58:00Z".parse().unwrap()),
            },
            last_error: None,
            api_url: Some("https://api.plow.co".into()),
            state_dir: "/Users/ada/.latch".into(),
            version: "0.1.0",
            protocol: 4,
            old_audit_log: None,
            page_isolation: None,
        },
        suggested: Some("studio".into()),
        api: Some("https://api.plow.co".into()),
        here_default: Some("studio".into()),
        serve: vec!["serve", "-d", "--again"],
        plow: Asked::Answered(directory(&[
            ("studio", true, Some(true)),
            ("pi", false, Some(false)),
        ])),
        home: Some("/Users/ada".into()),
        now: "2026-10-23T18:00:00Z[UTC]".parse().unwrap(),
    }
}

/// Plow's answer: these hosts (name, this machine, online), the default
/// host as `latch status` would pick it.
fn directory(hosts: &[(&str, bool, Option<bool>)]) -> Directory {
    let hosts: Vec<Host> = hosts
        .iter()
        .map(|&(name, this_machine, online)| Host {
            name: name.into(),
            key: format!("key-of-{name}"),
            this_machine,
            online,
            last_seen_at: online
                .is_some_and(|online| !online)
                .then(|| "2026-10-22T09:14:00Z".into()),
        })
        .collect();
    let (default_host, default_host_reason) = match hosts.as_slice() {
        [only] => (Some(only.name.clone()), None),
        [] => (None, Some("no_host".to_string())),
        _ => (None, Some("host_required".to_string())),
    };
    Directory {
        account: Account {
            owner_uid: "usr_1".into(),
            display_name: Some("Ada Lovelace".into()),
        },
        hosts,
        default_host,
        default_host_reason,
    }
}

/// The facts and what follows them, as a person reads them (no color).
fn shown(facts: &Facts) -> String {
    let rows = facts
        .rows(BESIDE)
        .into_iter()
        .map(|row| painted(&row, BESIDE));
    let closing = facts.closing().into_iter().map(|row| painted(&row, 80));
    rows.chain(closing)
        .map(|line| line.trim_end().to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn a_mac_serving_says_who_where_and_what_plow_says() {
    assert_eq!(
        shown(&facts()),
        "\
latch 0.1.0 (local build) · api.plow.co

Account    Ada Lovelace
Login      owner (device)

Hosts      this Mac: studio
           serving (background)
           Plow: online
           pi: not online, last seen Oct 22
Commands   go to studio
Connectors Google on · Messages on
           Contacts off
Update     up to date (checked 3:58 PM)
Folder     ~/.latch

As JSON: latch status. Live activity: latch dashboard."
            .replace("this Mac", &format!("this {}", here()))
    );
}

#[test]
fn before_login_it_suggests_a_name_and_says_to_set_up() {
    let mut facts = facts();
    facts.local.login = None;
    facts.local.this_machine.name = None;
    facts.local.this_machine.hosting = Hosting {
        running: false,
        ready: false,
        managed: false,
        state: Phase::Stopped,
        pid: None,
        ..facts.local.this_machine.hosting
    };
    facts.local.update.checked_at = None;
    facts.here_default = None;
    facts.plow = Asked::SignedOut;
    let text = shown(&facts);
    for row in [
        "Account    not signed in",
        "not signed in\n           suggested name: studio\n           stopped\n           sign in to see the others",
        "Update     not checked",
        "isn't signed in to Plow. Run: latch setup",
    ] {
        assert!(text.contains(row), "{row:?} in:\n{text}");
    }
    for row in ["Login", "Commands"] {
        assert!(!text.contains(row), "{row:?} in:\n{text}");
    }
}

#[test]
fn plow_rows_wait_then_say_plow_did_not_answer_never_logged_out_or_zero() {
    let mut facts = facts();
    facts.plow = Asked::Waiting;
    let waiting = shown(&facts);
    assert!(waiting.contains("Account    …"), "{waiting}");
    assert!(waiting.contains("Plow: …"), "{waiting}");
    // This machine has served: where commands go needs no Plow.
    assert!(waiting.contains("Commands   go to studio"), "{waiting}");

    facts.plow = Asked::NoAnswer;
    facts.here_default = None;
    let late = shown(&facts);
    assert!(late.contains("Account    Plow didn't answer"), "{late}");
    assert!(late.contains("Plow: didn't answer"), "{late}");
    assert!(late.contains("Commands   Plow didn't answer"), "{late}");
    assert!(
        !late.contains("not signed in") && !late.contains("no hosts"),
        "{late}"
    );

    facts.plow = Asked::Failed("Plow answered HTTP 401: this credential may not do that".into());
    let refused = shown(&facts);
    assert!(
        refused.contains("Account    Plow answered HTTP 401"),
        "{refused}"
    );
}

#[test]
fn several_hosts_and_never_served_needs_a_host_and_lists_three() {
    let mut facts = facts();
    facts.here_default = None;
    facts.plow = Asked::Answered(directory(&[
        ("a", false, Some(true)),
        ("b", false, Some(true)),
        ("c", false, None),
        ("d", false, Some(true)),
        ("e", false, Some(true)),
    ]));
    let text = shown(&facts);
    assert!(text.contains("Plow: not listed as a host"), "{text}");
    assert!(text.contains("           c: not online\n"), "{text}");
    assert!(text.contains("+2 more: latch status"), "{text}");
    assert!(!text.contains("d: online"), "{text}");
    assert!(
        text.contains("Commands   need --host: a, b, c, d, e"),
        "{text}"
    );
}

#[test]
fn an_agents_login_and_its_host() {
    let mut facts = facts();
    facts.local.login = Some(Login {
        kind: "agent".into(),
    });
    facts.local.this_machine.name = Some("research-bot".into());
    facts.local.plugins = json!([{"name": "contacts", "state": "needs_setup",
                                  "enable": "latch plugins enable contacts"}]);
    let text = shown(&facts);
    assert!(text.contains("Login      agent\n"), "{text}");
    assert!(text.contains(": research-bot"), "{text}");
    // Connectors are the owner's to set up.
    assert!(!text.contains("Run: latch plugins"), "{text}");
}

#[test]
fn the_next_step_is_the_first_thing_to_do() {
    let mut facts = facts();
    facts.local.update.available = true;
    facts.local.update.build = Some(142);
    assert!(shown(&facts).contains("Build 142 is available. Run: latch update"));
    assert!(shown(&facts).contains("Update     build 142 is available"));

    facts.local.this_machine.hosting.running = false;
    facts.local.this_machine.hosting.state = Phase::Stopped;
    assert!(shown(&facts).contains("isn't running on this"));
    assert!(shown(&facts).contains("Run: latch serve -d --again"));

    facts.local.plugins = json!([{"name": "contacts", "state": "needs_setup",
                                  "reason": "no Full Disk Access",
                                  "enable": "latch plugins enable contacts"}]);
    let text = shown(&facts);
    assert!(text.contains("Connectors Contacts needs setup"), "{text}");
    assert!(
        text.contains("The Contacts connector needs setup. Run: latch plugins enable contacts"),
        "{text}"
    );

    facts.local.this_machine.hosting.running = true;
    facts.local.this_machine.hosting.blocked = Some(Problem {
        code: "other_version".into(),
        message: "a host from another latch version is running (pid 4242); stop it".into(),
    });
    let text = shown(&facts);
    assert!(
        text.contains("blocked by another latch (pid 4242)"),
        "{text}"
    );
    assert!(
        text.contains("\na host from another latch version is running (pid 4242); stop it\n"),
        "{text}"
    );
}

#[test]
fn a_host_that_failed_says_why() {
    let mut facts = facts();
    facts.local.this_machine.hosting.running = false;
    facts.local.this_machine.hosting.state = Phase::Error;
    facts.local.last_error = Some(Problem {
        code: "serve_ended".into(),
        message: "latch serve ended without stopping: it was killed or crashed".into(),
    });
    facts.local.this_machine.hosting.deny_exec =
        Some(crate::serve::DenyExec::from_flags(&["*".to_string()]));
    let text = shown(&facts);
    assert!(
        text.contains("stopped after an error\n           latch serve ended without"),
        "{text}"
    );
    assert!(
        text.contains("Exec       agents can't run programs here"),
        "{text}"
    );
}

#[test]
fn names_from_plow_cannot_drive_the_terminal() {
    let mut facts = facts();
    facts.plow = Asked::Answered(directory(&[
        ("studio", true, Some(true)),
        ("evil\x1b[2J\nname", false, Some(true)),
    ]));
    let text = shown(&facts);
    assert!(!text.contains('\x1b'), "{text:?}");
    assert!(text.contains("evil [2J⏎name: online"), "{text}");
}

#[test]
fn rows_are_cut_at_the_edge_never_wrapped() {
    let mut facts = facts();
    facts.local.this_machine.name = Some("x".repeat(80));
    let rows = facts.rows(BESIDE);
    assert!(
        rows.iter()
            .all(|row| painted(row, BESIDE).chars().count() <= BESIDE)
    );
}

#[test]
fn the_mark_sits_beside_the_facts_in_80_columns_and_above_them_when_narrower() {
    let facts = facts();
    let beside = Screen::new(80, 24, true, &facts);
    assert_eq!((&beside.layout, beside.animate), (&Layout::Beside, true));
    let lines = beside.lines(&logo::finished(), &facts);
    assert!(
        lines.iter().all(|line| line.chars().count() <= 80),
        "{lines:#?}"
    );
    assert!(
        lines[0].ends_with("latch 0.1.0 (local build) · api.plow.co"),
        "{lines:#?}"
    );

    // Narrow and tall: the mark animates above the facts.
    let above = Screen::new(60, 50, true, &facts);
    assert_eq!(
        (&above.layout, above.animate, above.fixed),
        (&Layout::Above, true, false)
    );
    let lines = above.lines(&logo::finished(), &facts);
    assert!(
        lines[logo::ROWS + 1].starts_with("latch 0.1.0"),
        "{lines:#?}"
    );

    // Narrow and short: the still mark is drawn once, the facts under it
    // again as Plow answers.
    let short = Screen::new(60, 24, true, &facts);
    assert_eq!(
        (short.animate, short.fixed, short.at_end_only),
        (false, true, false)
    );

    // Without color it never moves.
    assert!(!Screen::new(120, 40, false, &facts).animate);
}

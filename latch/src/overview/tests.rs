use super::*;
use crate::lifecycle::Problem;
use crate::status::{Account, Host, Hosting};
use serde_json::json;

/// A Mac logged in as the account owner, serving in the background, Google and
/// Messages on, Contacts off, checked for updates this morning.
fn facts() -> Facts {
    Facts {
        login: Some(Login {
            kind: "device".into(),
        }),
        name: Some("studio".into()),
        plugins: json!([{"name": "google", "state": "enabled"},
                        {"name": "messages", "state": "enabled"},
                        {"name": "contacts", "state": "disabled",
                         "enable": "latch plugins enable contacts"}]),
        update: crate::update::Status {
            available: false,
            build: Some(140),
            checked_at: Some("2026-10-23T15:58:00Z".parse().unwrap()),
        },
        state_dir: "/Users/ada/.latch".into(),
        suggested: Some("studio".into()),
        api: Some("https://api.plow.co".into()),
        here_default: Some("studio".into()),
        serve: vec!["serve", "-d", "--again"],
        host: Pending::Got(Hosted {
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
            last_error: None,
        }),
        plow: Some(Pending::Got(directory(&[
            ("studio", true, Some(true)),
            ("pi", false, Some(false)),
        ]))),
        home: Some("/Users/ada".into()),
        now: "2026-10-23T18:00:00Z[UTC]".parse().unwrap(),
    }
}

/// The host's state, as the facts have it.
fn hosted(facts: &mut Facts) -> &mut Hosted {
    let Pending::Got(hosted) = &mut facts.host else {
        panic!("the host is known");
    };
    hosted
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
latch 0.1.0 (local build) · Plow: api.plow.co

Account    Ada Lovelace
Login      account owner

Hosts      this Mac: studio
           serving (background)
           Plow says: online
           pi: not online, last seen Oct 22
Commands   default host: studio
Connectors Google on · Messages on
           Contacts off
Update     up to date (checked 3:58 PM)
Folder     ~/.latch

For agents (JSON): latch status. Live activity: latch dashboard."
            .replace("this Mac", &format!("this {}", here()))
    );
}

#[test]
fn before_login_it_suggests_a_name_and_says_to_set_up() {
    let mut facts = facts();
    facts.login = None;
    facts.name = None;
    let hosting = &mut hosted(&mut facts).hosting;
    (hosting.running, hosting.ready, hosting.managed) = (false, false, false);
    (hosting.state, hosting.pid) = (Phase::Stopped, None);
    facts.update.checked_at = None;
    facts.here_default = None;
    facts.plow = None;
    let text = shown(&facts);
    for row in [
        "Account    not logged in",
        "not logged in\n           name at login: studio\n           stopped\n           log in to see other hosts",
        "Update     not checked",
        "isn't logged in to Plow. Run: latch setup",
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
    facts.plow = Some(Pending::Waiting);
    let waiting = shown(&facts);
    assert!(waiting.contains("Account    …"), "{waiting}");
    assert!(waiting.contains("Plow says: …"), "{waiting}");
    // This machine has served: where commands go needs no Plow.
    assert!(
        waiting.contains("Commands   default host: studio"),
        "{waiting}"
    );

    facts.plow = Some(Pending::NoAnswer);
    facts.here_default = None;
    let late = shown(&facts);
    assert!(late.contains("Account    Plow didn't answer"), "{late}");
    assert!(late.contains("           Plow didn't answer\n"), "{late}");
    assert!(late.contains("Commands   Plow didn't answer"), "{late}");
    assert!(
        !late.contains("not logged in") && !late.contains("no hosts"),
        "{late}"
    );

    facts.plow = Some(Pending::Failed(
        "Plow answered HTTP 401: this credential may not do that".into(),
    ));
    let refused = shown(&facts);
    assert!(
        refused.contains("Account    Plow answered HTTP 401: this crede…"),
        "{refused}"
    );
    // Said once: the other rows point to it.
    assert_eq!(refused.matches("HTTP 401").count(), 1, "{refused}");
    assert!(
        refused.contains("           Plow error (see Account)\n"),
        "{refused}"
    );
    assert!(
        refused.contains("Commands   Plow error (see Account)"),
        "{refused}"
    );
}

#[test]
fn the_host_row_waits_for_the_service_manager_and_never_guesses() {
    let mut facts = facts();
    facts.host = Pending::Waiting;
    let waiting = shown(&facts);
    assert!(waiting.contains(": studio\n           …\n"), "{waiting}");
    facts.host = Pending::NoAnswer;
    let late = shown(&facts);
    assert!(
        late.contains("state unknown: the check timed out"),
        "{late}"
    );
    // Unknown is not stopped: no step to start it, and no Exec row.
    assert!(
        !late.contains("Run: latch serve") && !late.contains("Exec"),
        "{late}"
    );
}

#[test]
fn several_hosts_and_never_served_needs_a_host_and_lists_three() {
    let mut facts = facts();
    facts.here_default = None;
    facts.plow = Some(Pending::Got(directory(&[
        ("a", false, Some(true)),
        ("b", false, Some(true)),
        ("c", false, None),
        ("d", false, Some(true)),
        ("e", false, Some(true)),
    ])));
    let text = shown(&facts);
    assert!(text.contains("Plow says: not listed as a host"), "{text}");
    assert!(text.contains("           c: not online\n"), "{text}");
    assert!(text.contains("+2 more: latch status"), "{text}");
    assert!(!text.contains("d: online"), "{text}");
    assert!(
        text.contains("Commands   choose with --host: a, b, c, d, e"),
        "{text}"
    );
}

#[test]
fn an_agents_login_and_its_host() {
    let mut facts = facts();
    facts.login = Some(Login {
        kind: "agent".into(),
    });
    facts.name = Some("research-bot".into());
    facts.plugins = json!([{"name": "contacts", "state": "needs_setup",
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
    facts.update.available = true;
    facts.update.build = Some(142);
    assert!(shown(&facts).contains("Build 142 is available. Run: latch update"));
    assert!(shown(&facts).contains("Update     build 142 is available"));

    hosted(&mut facts).hosting.running = false;
    hosted(&mut facts).hosting.state = Phase::Stopped;
    assert!(shown(&facts).contains("isn't running on this"));
    assert!(shown(&facts).contains("Run: latch serve -d --again"));

    facts.plugins = json!([{"name": "contacts", "state": "needs_setup",
                                  "reason": "no Full Disk Access",
                                  "enable": "latch plugins enable contacts"}]);
    let text = shown(&facts);
    assert!(text.contains("Connectors Contacts needs setup"), "{text}");
    assert!(
        text.contains("The Contacts connector needs setup. Run: latch plugins enable contacts"),
        "{text}"
    );

    hosted(&mut facts).hosting.running = true;
    hosted(&mut facts).hosting.blocked = Some(Problem {
        code: "other_version".into(),
        message: "a host from another latch version is running (pid 4242); stop it".into(),
    });
    let text = shown(&facts);
    assert!(
        text.contains("blocked by another latch version (…"),
        "{text}"
    );
    let step: String = facts
        .next_step()
        .unwrap()
        .into_iter()
        .map(|(text, _)| text)
        .collect();
    assert_eq!(
        step,
        format!(
            "Another latch version is running on this {}. Stop it: Ctrl-C in its terminal, or kill 4242.",
            here()
        )
    );
}

#[test]
fn a_host_that_failed_says_why() {
    let mut facts = facts();
    hosted(&mut facts).hosting.running = false;
    hosted(&mut facts).hosting.state = Phase::Error;
    hosted(&mut facts).last_error = Some(Problem {
        code: "serve_ended".into(),
        message: "latch serve ended without stopping: it was killed or crashed".into(),
    });
    hosted(&mut facts).hosting.deny_exec =
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
    facts.plow = Some(Pending::Got(directory(&[
        ("studio", true, Some(true)),
        ("evil\x1b[2J\nname", false, Some(true)),
    ])));
    let text = shown(&facts);
    assert!(!text.contains('\x1b'), "{text:?}");
    assert!(text.contains("evil [2J⏎name: online"), "{text}");
}

#[test]
fn rows_are_cut_at_the_edge_never_wrapped() {
    let mut facts = facts();
    facts.name = Some("x".repeat(80));
    let rows: Vec<String> = facts
        .rows(BESIDE)
        .iter()
        .map(|row| painted(row, BESIDE))
        .collect();
    assert!(
        rows.iter().all(|row| row.chars().count() <= BESIDE),
        "{rows:#?}"
    );
    let long = rows.iter().find(|row| row.contains("xxx")).unwrap();
    assert_eq!(long.chars().count(), BESIDE, "{long}");
    assert!(long.ends_with("x…"), "{long}");
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
        lines[0].ends_with("latch 0.1.0 (local build) · Plow: api.plow.co"),
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

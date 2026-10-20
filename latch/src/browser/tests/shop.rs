//! The plan suite: development cases on Greenleaf Market, a fictional shop
//! in `tests/browser-shop`, listed in its `suite.json`.
//!
//! Each case names how to set the shop up (`setup`, a `reset.html` URL),
//! where the plan starts, its short-form steps (`{site}` is the shop's
//! origin), its facts and the vault items it needs, the outcome latch must
//! give, and an independent checker: a JavaScript expression on the final
//! page that knows the right item, not just the right kind of page. A case
//! that stops for a confirmation may go on (`then`): something on the
//! page changes (`before`, as the shop's server might), the agent
//! confirms with `do --confirm C`, and that has its own outcome and
//! checker. A sign-in wall
//! stops `needs_login` until plans can sign in.
//!
//! The checkers are tested here against states set up directly
//! (`probes`): each holds on its right state and fails on the near misses
//! next to it, and the fixtures behave as their cases need.

use std::collections::{BTreeMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

use super::{HEADLESS, latch, test_state};
use crate::browser::plan::{self, Plan};
use crate::browser::session::Session;

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub(super) struct Case {
    pub name: String,
    pub setup: String,
    pub start: String,
    pub steps: Vec<String>,
    #[serde(default)]
    pub facts: BTreeMap<String, String>,
    /// Made-up vault items the case needs, by kind: `card`.
    #[serde(default)]
    pub vault: Vec<String>,
    pub expected: Expected,
    pub check: String,
    #[serde(default)]
    pub then: Option<Then>,
    pub probes: Vec<Probe>,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub(super) struct Expected {
    pub status: String,
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub step: Option<usize>,
    /// Exactly the steps that end `already_held`.
    #[serde(default)]
    pub held: Vec<usize>,
    #[serde(default)]
    pub review: bool,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub(super) struct Then {
    #[serde(default)]
    pub before: Option<String>,
    pub expected: Expected,
    pub check: String,
}

/// A state set up directly, and whether a checker holds on it: the case's
/// own, or `check`.
#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub(super) struct Probe {
    pub setup: String,
    pub at: String,
    #[serde(default)]
    pub script: Option<String>,
    #[serde(default)]
    pub check: Option<String>,
    pub holds: bool,
}

/// What every checker may use: the shop's state, read from its storage,
/// and the page's address and fields.
const PRELUDE: &str = "\
const cart = JSON.parse(localStorage.getItem('cart') || '[]');
const order = JSON.parse(localStorage.getItem('order') || 'null');
const user = JSON.parse(localStorage.getItem('user') || 'null');
const only = (id, size, qty) => cart.length === 1 && cart[0].id === id && cart[0].size === size && cart[0].qty === qty;
const page = location.pathname.split('/').pop();
const q = (key) => new URLSearchParams(location.search).get(key);
const value = (id) => document.getElementById(id) ? document.getElementById(id).value : null;";

/// `check` as one script whose value is whether it holds.
pub(super) fn checker(check: &str) -> String {
    format!("(() => {{ {PRELUDE}\nreturn ({check}) === true; }})()")
}

fn folder() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/browser-shop")
}

pub(super) fn suite() -> Vec<Case> {
    let text = std::fs::read_to_string(folder().join("suite.json")).unwrap();
    serde_json::from_str(&text).unwrap()
}

/// A case's steps as the plan the agent would send, with `{site}` filled.
pub(super) fn plan_of(case: &Case, site: &str) -> Result<Plan, String> {
    let plan = Plan {
        steps: case
            .steps
            .iter()
            .map(|step| plan::short_step(&step.replace("{site}", site)))
            .collect::<Result<_, _>>()?,
        facts: case.facts.clone(),
        ..Plan::default()
    };
    plan.validate()?;
    Ok(plan)
}

/// Serves the shop on 127.0.0.1 for as long as the test runs; returns its
/// origin.
pub(super) fn serve_shop() -> String {
    let root = folder();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for socket in listener.incoming() {
            let Ok(mut socket) = socket else { continue };
            let mut line = String::new();
            BufReader::new(&socket).read_line(&mut line).unwrap_or(0);
            let path = line.split(' ').nth(1).unwrap_or("/");
            let path = path
                .split('?')
                .next()
                .unwrap_or("/")
                .trim_start_matches('/');
            let file = root.join(path);
            let found = (!path.split('/').any(|part| part == ".."))
                .then(|| std::fs::read(&file).ok())
                .flatten();
            let kind = match file.extension().and_then(|e| e.to_str()) {
                Some("js") => "text/javascript",
                Some("css") => "text/css",
                Some("json") => "application/json",
                _ => "text/html; charset=utf-8",
            };
            let (status, body) = match &found {
                Some(body) => ("200 OK", body.as_slice()),
                None => ("404 Not Found", &b""[..]),
            };
            let head = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = socket.write_all(head.as_bytes());
            let _ = socket.write_all(body);
        }
    });
    format!("http://127.0.0.1:{port}")
}

/// The stop codes a plan can end with (exit 1 and exit 4).
const STOP_CODES: [&str; 29] = [
    "check_not_met",
    "unconfirmed",
    "already_true",
    "needs_text",
    "needs_fields",
    "needs_login",
    "needs_owner",
    "blocked",
    "stuck",
    "repeat",
    "no_progress",
    "ambiguous_target",
    "step_budget",
    "plan_budget",
    "deadline",
    "stale_snapshot",
    "covered_target",
    "session_paused",
    "site_busy",
    "model_unavailable",
    "no_such_session",
    "unsupported_capability",
    "timeout",
    "confirm_needed",
    "confirm_stale",
    "step_not_allowed",
    "not_owner",
    "origin_mismatch",
    "secret_on_page",
];

fn expected_is_sound(name: &str, expected: &Expected, steps: usize) {
    match expected.status.as_str() {
        "reached" | "dispatched" => assert!(
            expected.code.is_none() && expected.step.is_none(),
            "{name}: only a stop has a code and a step"
        ),
        "stopped" => {
            assert!(
                !expected.review,
                "{name}: only a result that reached is for review"
            );
            if let Some(code) = &expected.code {
                assert!(
                    STOP_CODES.contains(&code.as_str()),
                    "{name}: no stop code {code}"
                );
            }
        }
        other => panic!("{name}: no status {other}"),
    }
    for n in expected.step.iter().chain(&expected.held) {
        assert!((1..=steps).contains(n), "{name}: there is no step {n}");
    }
}

#[test]
fn the_plan_suite_is_well_formed() {
    let cases = suite();
    let mut names = HashSet::new();
    for case in &cases {
        let name = &case.name;
        assert!(names.insert(name.clone()), "{name}: named twice");
        let plan =
            plan_of(case, "http://127.0.0.1:1").unwrap_or_else(|why| panic!("{name}: {why}"));
        expected_is_sound(name, &case.expected, plan.steps.len());
        if let Some(then) = &case.then {
            assert_eq!(
                case.expected.code.as_deref(),
                Some("confirm_needed"),
                "{name}: only a confirmation goes on"
            );
            expected_is_sound(name, &then.expected, 1);
            assert!(
                case.probes
                    .iter()
                    .any(|p| p.holds && p.check.as_deref() == Some(&then.check)),
                "{name}: its confirmation's checker needs a state it holds on"
            );
            if let Some(before) = &then.before {
                assert!(
                    case.probes.iter().any(|p| p
                        .script
                        .as_ref()
                        .is_some_and(|s| s.contains(before.as_str()))),
                    "{name}: a probe shows what its change does to the page"
                );
            }
        }
        for kind in &case.vault {
            assert_eq!(kind, "card", "{name}: no made-up vault item {kind}");
        }
        assert!(
            case.probes.iter().any(|p| p.check.is_none() && p.holds)
                && case.probes.iter().any(|p| p.check.is_none() && !p.holds),
            "{name}: its checker needs a state it holds on and a near miss it fails on"
        );
    }
    // The ` -> ` in a quoted value is the value's, not a check.
    let arrow = cases.iter().find(|c| c.name == "arrow-in-a-value").unwrap();
    let step = &plan_of(arrow, "").unwrap().steps[0];
    assert_eq!(step.values.values().collect::<Vec<_>>(), ["A -> B"]);
    assert!(step.check.is_none());
    // A URL is offered to `navigate` only when it is quoted.
    let site = "http://127.0.0.1:1";
    for (name, offered) in [("navigate-quoted", true), ("navigate-unquoted", false)] {
        let case = cases.iter().find(|c| c.name == name).unwrap();
        let step = &plan_of(case, site).unwrap().steps[0];
        assert_eq!(!step.urls.is_empty(), offered, "{name}");
    }
}

#[tokio::test]
async fn each_checker_tells_its_right_state_from_the_near_misses() {
    let (_dir, state, _area) = test_state();
    let site = serve_shop();
    let mut session = Session::start_with(
        &latch(),
        &state,
        "b5c-shop",
        Duration::from_secs(300),
        HEADLESS,
    )
    .await
    .unwrap();
    let mut wrong = Vec::new();
    for case in suite() {
        // Every case starts on a page the shop has.
        for url in [&case.setup, &case.start] {
            let page = session
                .call(json!({"op": "open", "url": format!("{site}/{url}")}))
                .await
                .unwrap_or_else(|f| panic!("{}: {url}: {}", case.name, f.message))
                .result;
            assert!(
                !page["title"].as_str().unwrap_or_default().is_empty(),
                "{}: {url} has no page",
                case.name
            );
        }
        for (n, probe) in case.probes.iter().enumerate() {
            for url in [&probe.setup, &probe.at] {
                session
                    .call(json!({"op": "open", "url": format!("{site}/{url}")}))
                    .await
                    .unwrap_or_else(|f| panic!("{} probe {n}: {url}: {}", case.name, f.message));
            }
            if let Some(script) = &probe.script {
                session
                    .call(
                        json!({"op": "eval", "script": format!("(async () => {{ {script}; }})()")}),
                    )
                    .await
                    .unwrap_or_else(|f| panic!("{} probe {n}: {}", case.name, f.message));
            }
            let check = probe.check.as_deref().unwrap_or(&case.check);
            let held = session
                .call(json!({"op": "eval", "script": checker(check)}))
                .await
                .unwrap_or_else(|f| panic!("{} probe {n}: {}", case.name, f.message))
                .result["value"]
                == true;
            println!("{} probe {n} ({}): holds {held}", case.name, probe.at);
            if held != probe.holds {
                wrong.push(format!(
                    "{} probe {n} at {}: holds {held}",
                    case.name, probe.at
                ));
            }
        }
    }
    session.close().await.unwrap();
    assert!(
        wrong.is_empty(),
        "checkers that judged a probe wrongly: {wrong:#?}"
    );
}

#[tokio::test]
async fn the_view_shows_what_a_decision_needs() {
    let (_dir, state, _area) = test_state();
    let site = serve_shop();
    let mut session = Session::start_with(
        &latch(),
        &state,
        "b5c-view",
        Duration::from_secs(300),
        HEADLESS,
    )
    .await
    .unwrap();
    let mut call = async |call: serde_json::Value| {
        session
            .call(call)
            .await
            .unwrap_or_else(|f| panic!("{}", f.message))
            .result
    };
    let open = |page: &str| json!({"op": "open", "url": format!("{site}/{page}")});
    let view = json!({"op": "view"});
    let named = |view: &serde_json::Value, role: &str, name: &str| {
        view["elements"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["role"] == role && e["name"] == name)
            .cloned()
            .unwrap_or_else(|| panic!("no {role} {name:?} in {:#}", view["elements"]))
    };

    // A dropdown is named by its label alone, never its options, and lists
    // them; its value is a field line.
    call(open("reset.html")).await;
    let opened = call(open("product.html?id=greek")).await;
    let product = call(view.clone()).await;
    let size = named(&product, "combobox", "Size");
    assert_eq!(size["options"], json!(["Small", "Regular", "Large"]));
    assert_eq!(size["kind"], "select");
    assert_eq!(named(&product, "spinbutton", "Quantity")["kind"], "text");
    assert!(
        product["fields"]
            .as_array()
            .unwrap()
            .contains(&json!("combobox \"Size\": \"Regular\"")),
        "{:#}",
        product["fields"]
    );
    assert_eq!(product["scroll"]["y"], 0);
    assert!(product["wall"].is_null());
    // The view is latch's own: a ref without a snapshot is still read in
    // the page the agent was shown.
    let pinned = call(json!({"op": "pin", "target": "e1"})).await;
    assert_eq!(pinned["snapshot"], opened["snapshot"]);

    // The lines on screen keep a sentence whole across its elements.
    call(open(
        "reset.html?user=testuser@example.com&cart=greek:Regular",
    ))
    .await;
    call(open("checkout.html")).await;
    let checkout = call(view.clone()).await;
    assert!(
        checkout["on_screen"]
            .as_array()
            .unwrap()
            .contains(&json!("1 item(s), total $13.74 including $3.99 delivery.")),
        "{:#}",
        checkout["on_screen"]
    );

    // A cookie modal covers the page behind it, not its own buttons.
    call(open("reset.html")).await;
    call(open("index.html?cookies=1")).await;
    let covered = call(view.clone()).await;
    assert!(named(&covered, "button", "Accept all cookies")["covered"].is_null());
    let behind = covered["elements"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["covered"] == true)
        .count();
    assert!(behind > 0, "{:#}", covered["elements"]);

    // The page's state holds still on an unchanged page, and changes with
    // the scroll position, and with a same-URL wizard's next stage.
    call(open("reset.html")).await;
    call(open("wizard.html")).await;
    let first = call(view.clone()).await;
    let again = call(view.clone()).await;
    assert_eq!(first["state"], again["state"]);
    let next = named(&again, "button", "Next");
    call(json!({"op": "act", "action": "click", "quiet": true,
                "snapshot": again["snapshot"], "target": next["ref"]}))
    .await;
    let stage = call(view.clone()).await;
    assert_eq!(stage["url"], first["url"]);
    assert_ne!(stage["state"], first["state"]);
    call(open("reset.html")).await;
    call(open("category.html?cat=salads")).await;
    call(json!({"op": "eval", "script": "document.body.style.minHeight = '4000px'"})).await;
    let top = call(view.clone()).await;
    call(json!({"op": "act", "action": "scroll", "quiet": true, "text": "down"})).await;
    let scrolled = call(view.clone()).await;
    assert!(scrolled["scroll"]["y"].as_i64().unwrap() > 0);
    assert_ne!(top["state"], scrolled["state"]);
    drop(call);
    session.close().await.unwrap();
}

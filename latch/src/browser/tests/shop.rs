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
    /// Stop codes any one of which is the listed outcome.
    #[serde(default)]
    pub codes: Vec<String>,
    #[serde(default)]
    pub step: Option<usize>,
    /// The most actions the plan may have sent.
    #[serde(default)]
    pub max_actions: Option<usize>,
    /// Another outcome that is also the listed one.
    #[serde(default)]
    pub or: Option<Box<Expected>>,
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
    let mut plan = plan;
    plan.resolve_facts();
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
            expected.code.is_none() && expected.codes.is_empty() && expected.step.is_none(),
            "{name}: only a stop has a code and a step"
        ),
        "stopped" => {
            assert!(
                !expected.review,
                "{name}: only a result that reached is for review"
            );
            assert!(
                expected.code.is_none() || expected.codes.is_empty(),
                "{name}: one code, or a list of codes"
            );
            for code in expected.code.iter().chain(&expected.codes) {
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
    if let Some(or) = &expected.or {
        assert!(or.or.is_none(), "{name}: one other outcome at most");
        expected_is_sound(name, or, steps);
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
    let (_dir, state, _area) = test_state("cft", &["b5c-shop"]);
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
    let (_dir, state, _area) = test_state("cft", &["b5c-view"]);
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

/// What one case came to: its outcome as the plan answered, and whether
/// its checker held on the page then.
fn outcome_of(
    result: Result<serde_json::Value, crate::failure::Failure>,
) -> (serde_json::Value, serde_json::Value) {
    let (answer, code) = match result {
        Ok(answer) => (answer, None),
        Err(failure) => (failure.details.clone(), Some(failure.code)),
    };
    let held: Vec<u64> = answer["steps"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|s| s["outcome"] == "already_held")
        .filter_map(|s| s["step"].as_u64())
        .collect();
    let got = json!({
        "status": answer["status"],
        "code": code,
        "step": answer["stop"]["step"],
        "held": held,
        "review": answer["review"] == true,
        "actions": answer["trail"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|t| t["result"] == "dispatched")
            .count(),
    });
    (got, answer)
}

/// Whether `check` holds on the session's page now.
async fn holds(h: &super::steps::Harness, check: &str) -> bool {
    let (value, _) = crate::browser::steps::call(
        &h.entry,
        json!({"op": "eval", "script": checker(check)}),
        &mut serde_json::Map::new(),
    )
    .await
    .unwrap();
    value["value"] == true
}

/// Where `got` differs from `expected`: nothing if it matches its other
/// outcome instead.
fn differences(expected: &Expected, got: &serde_json::Value) -> Vec<String> {
    let wrong = differences_from(expected, got);
    match &expected.or {
        Some(or) if differences_from(or, got).is_empty() => Vec::new(),
        _ => wrong,
    }
}

fn differences_from(expected: &Expected, got: &serde_json::Value) -> Vec<String> {
    let mut wrong = Vec::new();
    if got["status"] != expected.status.as_str() {
        wrong.push(format!(
            "status {} (expected {})",
            got["status"], expected.status
        ));
    }
    if let Some(code) = &expected.code
        && got["code"] != code.as_str()
    {
        wrong.push(format!("code {} (expected {code})", got["code"]));
    }
    if !expected.codes.is_empty()
        && !expected
            .codes
            .iter()
            .any(|code| got["code"] == code.as_str())
    {
        wrong.push(format!(
            "code {} (expected one of {:?})",
            got["code"], expected.codes
        ));
    }
    if let Some(most) = expected.max_actions
        && got["actions"].as_u64().unwrap_or(0) > most as u64
    {
        wrong.push(format!(
            "{} actions sent (expected at most {most})",
            got["actions"]
        ));
    }
    if let Some(step) = expected.step
        && got["step"] != step
    {
        wrong.push(format!("stopped at step {} (expected {step})", got["step"]));
    }
    if got["held"] != json!(expected.held) {
        wrong.push(format!(
            "held {} (expected {:?})",
            got["held"], expected.held
        ));
    }
    if expected.status == "reached" && got["review"] != expected.review {
        wrong.push(format!(
            "review {} (expected {})",
            got["review"], expected.review
        ));
    }
    wrong
}

/// The most decision-model calls the suite makes in a minute.
const SUITE_PER_MINUTE: usize = 30;

/// The plan suite on the real decision model: each case's plan through the
/// host's own `do`, with the model reached through Plow's decision route
/// on the login of the host in `LATCH_SUITE_HOST_STATE` (a dev Plow stack's:
/// prod Plow has no such route). Each run's cases, audit records and
/// metrics go to a new folder under `LATCH_SUITE_OUT`.
#[tokio::test]
#[ignore = "calls the decision model through a dev Plow stack: `just plan-suite`"]
async fn the_plan_suite_on_the_decision_model() {
    use super::steps::Harness;
    use crate::browser::model::Plow;
    use crate::vault::item::{CARD, Item, Raw};
    use crate::vault::store::Backend;

    let host = std::env::var("LATCH_SUITE_HOST_STATE").expect("LATCH_SUITE_HOST_STATE");
    let out = PathBuf::from(std::env::var("LATCH_SUITE_OUT").expect("LATCH_SUITE_OUT"));
    let plow = Plow::for_host(Path::new(&host)).unwrap();
    let run = out.join(
        jiff::Timestamp::now()
            .strftime("%Y%m%dT%H%M%SZ")
            .to_string(),
    );
    std::fs::create_dir_all(&run).unwrap();
    let (_dir, state, _area) = test_state("cft", &["default"]);
    let site = serve_shop();
    let mut h = Harness::start(&state, Some(std::sync::Arc::new(plow))).await;
    let card = h
        .browsers
        .vault
        .with(|s| {
            s.add(Item {
                id: String::new(),
                revision: 0,
                sites: Vec::new(),
                other_top_sites: Vec::new(),
                raw: Raw::from_labels(
                    CARD,
                    "Test Visa",
                    &[
                        ("cardholder name".into(), "Ada Lovelace".into()),
                        ("number".into(), "4111111111111111".into()),
                        ("expiry month".into(), "4".into()),
                        ("expiry year".into(), "2031".into()),
                        ("code".into(), "123".into()),
                    ],
                )
                .unwrap(),
            })
        })
        .unwrap()
        .item;
    let mut results = Vec::new();
    let mut audit_lines = String::new();
    let mut failed = Vec::new();
    // Plow's route takes 60 calls a minute from one login: the suite keeps
    // under it, so no case meets its limit. How long to wait, once `made`
    // more calls are counted.
    let mut calls: Vec<std::time::Instant> = Vec::new();
    let pace = |calls: &mut Vec<std::time::Instant>, made: usize| {
        let now = std::time::Instant::now();
        calls.extend(std::iter::repeat_n(now, made));
        calls.retain(|at| now.duration_since(*at) < Duration::from_secs(60));
        match calls.len().checked_sub(SUITE_PER_MINUTE) {
            Some(over) => Duration::from_secs(60) - now.duration_since(calls[over]),
            None => Duration::ZERO,
        }
    };
    let made = |h: &Harness| {
        h.audits
            .last()
            .and_then(|a| {
                a.get("model_calls")
                    .and_then(|c| c.as_array())
                    .map(Vec::len)
            })
            .unwrap_or(0)
    };
    // `LATCH_SUITE_CASES=a,b` runs only those cases.
    let only: Vec<String> = std::env::var("LATCH_SUITE_CASES")
        .map(|names| names.split(',').map(str::to_string).collect())
        .unwrap_or_default();
    for case in suite() {
        if !only.is_empty() && !only.contains(&case.name) {
            continue;
        }
        h.open(&format!("{site}/{}", case.setup)).await;
        h.open(&format!("{site}/{}", case.start)).await;
        let mut plan = plan_of(&case, &site).unwrap();
        if case.vault.iter().any(|kind| kind == "card") {
            plan.items = vec![card.clone()];
        }
        let started = std::time::Instant::now();
        let (got, answer) = outcome_of(h.send(json!({"plan": plan})).await);
        let checked = holds(&h, &case.check).await;
        let mut wrong = differences(&case.expected, &got);
        if !checked {
            wrong.push("its checker does not hold".into());
        }
        let mut record = json!({"name": case.name, "expected": {
            "status": case.expected.status, "code": case.expected.code, "codes": case.expected.codes,
            "max_actions": case.expected.max_actions, "step": case.expected.step,
            "held": case.expected.held, "review": case.expected.review},
            "got": got, "checker": checked, "seconds": started.elapsed().as_secs_f64(),
            "answer": answer});
        let mut audits = vec![h.audits.last().cloned().unwrap_or_default()];
        tokio::time::sleep(pace(&mut calls, made(&h))).await;
        if let Some(then) = &case.then
            && got["code"] == "confirm_needed"
        {
            if let Some(before) = &then.before {
                crate::browser::steps::call(
                    &h.entry,
                    json!({"op": "eval", "script": format!("(async () => {{ {before}; }})()")}),
                    &mut serde_json::Map::new(),
                )
                .await
                .unwrap();
                tokio::time::sleep(Duration::from_millis(600)).await;
            }
            let id = answer["stop"]["confirmation"]["id"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let (then_got, then_answer) = outcome_of(h.confirm(&id).await);
            let then_checked = holds(&h, &then.check).await;
            let then_wrong = differences(&then.expected, &then_got);
            for difference in then_wrong {
                wrong.push(format!("after confirming: {difference}"));
            }
            if !then_checked {
                wrong.push("after confirming: its checker does not hold".into());
            }
            record["then"] =
                json!({"got": then_got, "checker": then_checked, "answer": then_answer});
            audits.push(h.audits.last().cloned().unwrap_or_default());
            tokio::time::sleep(pace(&mut calls, made(&h))).await;
        }
        for mut audit in audits {
            audit.insert("command".into(), json!("do"));
            audit.insert("session".into(), json!(case.name));
            audit.insert("turn".into(), json!(case.name));
            let line = json!({"time": jiff::Timestamp::now().to_string(), "caller": "suite",
                              "agent": null, "op": "browser", "args": audit, "decision": "allow",
                              "outcome": "ok", "prev": "", "hash": ""});
            audit_lines.push_str(&format!("{line}\n"));
        }
        record["ok"] = json!(wrong.is_empty());
        record["wrong"] = json!(wrong);
        println!(
            "{}: {}",
            case.name,
            if wrong.is_empty() {
                "ok".to_string()
            } else {
                wrong.join("; ")
            }
        );
        if !wrong.is_empty() {
            failed.push(case.name.clone());
        }
        results.push(record);
        // Kept as it goes, so a run that ends early keeps what it did.
        std::fs::write(
            run.join("cases.json"),
            serde_json::to_string_pretty(&results).unwrap(),
        )
        .unwrap();
    }
    std::fs::write(
        run.join("cases.json"),
        serde_json::to_string_pretty(&results).unwrap(),
    )
    .unwrap();
    std::fs::write(run.join("audit.ndjson"), audit_lines).unwrap();
    println!("run kept in {}", run.display());
    assert!(
        failed.is_empty(),
        "cases that did not end as listed: {failed:?}"
    );
}

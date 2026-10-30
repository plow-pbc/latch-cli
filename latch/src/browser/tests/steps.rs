//! The step executor on the pinned engine, through the host's own `do`
//! (and `act`, a plan of one step), on the test site's expense, checkout
//! and counter pages: literal steps and their checks, confirmations, the
//! decision model's absence, and the journal.
//!
//! The decision model is a fake provider answering from fixed facts about
//! the test site, in every run, with a switch that makes it unreachable;
//! built only with `--features live-model` (`just gatekeeper`), the
//! confirmation task runs on the real model, through Plow.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Map, Value, json};

use super::{CALLER, find, latch, test_state};
use crate::browser::client::{act_literal, do_plan};
use crate::browser::host::{self, Browsers};
use crate::browser::plan::{Check, Plan, Step};
use crate::browser::steps;
use crate::cli::ActAction;
use crate::failure::{Exit, Failure};
use crate::model::{Answer, Asked, CallFailed, CallMeta, Kind, Provider, Question};
use crate::vault::host::Vault;

conformance!(
    literal_steps_take_effect_and_their_checks_decide_the_outcome,
    consequential_actions_stop_for_a_confirmation_bound_to_what_was_reviewed,
    with_no_model_clicks_and_enter_stop_and_issued_confirmations_still_dispatch,
    the_journal_outlives_a_host_restart_and_a_lost_reply_is_never_resent,
    a_key_on_an_element_that_cannot_take_the_focus_is_refused,
    typing_a_code_reaches_the_fields_one_time_code_pages_use,
    a_return_goes_through_selection_cards_in_batched_plans,
    a_container_drawn_over_its_target_still_covers_it,
    waiting_for_the_page_counts_as_settle_time,
);

/// The test site, plus the answers to its expense and order forms.
pub(super) fn site() -> String {
    let folder = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/browser-site");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for socket in listener.incoming() {
            let Ok(socket) = socket else { continue };
            let folder = folder.clone();
            std::thread::spawn(move || {
                let mut reader = BufReader::new(socket.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap_or(0);
                let mut length = 0;
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).unwrap_or(0) == 0 || header == "\r\n" {
                        break;
                    }
                    if let Some(v) = header.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = v.trim().parse().unwrap_or(0);
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap_or(());
                let body = String::from_utf8_lossy(&body).replace('+', " ");
                let field = |name: &str| {
                    body.split('&')
                        .find_map(|pair| pair.strip_prefix(&format!("{name}=")))
                        .unwrap_or_default()
                        .to_string()
                };
                let path = line.split(' ').nth(1).unwrap_or("/");
                let path = path.split('?').next().unwrap_or("/").to_string();
                let answer = |title: &str, text: String| {
                    format!("<!doctype html><meta charset=\"utf-8\"><title>{title}</title><h1>{title}</h1><p>{text}</p>")
                        .into_bytes()
                };
                let page = match path.as_str() {
                    "/expense-save" => Some(answer(
                        "Expense · Expenses",
                        format!("Draft saved. Amount: {}", field("amount")),
                    )),
                    "/placed" => Some(answer("Order · Shop", "Order placed.".into())),
                    file => std::fs::read(folder.join(file.trim_start_matches('/'))).ok(),
                };
                let mut socket = socket;
                let (status, page) = match page {
                    Some(page) => ("200 OK", page),
                    None => ("404 Not Found", Vec::new()),
                };
                let kind = if path.ends_with(".csv") {
                    "text/csv"
                } else {
                    "text/html"
                };
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    page.len()
                );
                let _ = socket.write_all(head.as_bytes());
                let _ = socket.write_all(&page);
            });
        }
    });
    format!("http://127.0.0.1:{port}")
}

/// A decision model that answers from fixed facts about the test site, so
/// the model's paths run in every `just test`: which actions can't be
/// undone, which page line proves each sentence, and which sentences are
/// vague. `down` makes it unreachable.
#[derive(Default)]
struct Fake {
    down: AtomicBool,
    /// Milliseconds each risk answer takes.
    slow: AtomicU64,
    /// When set, how many more calls it answers before it goes down.
    down_after: Mutex<Option<usize>>,
}

const FAKE_RISKY: [&str; 1] = ["Place order"];
const FAKE_FACTS: [(&str, &str); 2] = [
    ("the draft was saved", "Draft saved"),
    ("the total is $100.00", "$100.00"),
];
const FAKE_VAGUE: [&str; 1] = ["finish the expense report"];

impl Provider for Fake {
    fn ask<'a>(&'a self, state: &'a Value, questions: &'a [Question]) -> Asked<'a> {
        if let Some(left) = self.down_after.lock().unwrap().as_mut() {
            match left.checked_sub(1) {
                Some(fewer) => *left = fewer,
                None => self.down.store(true, Ordering::SeqCst),
            }
        }
        if self.down.load(Ordering::SeqCst) {
            return Box::pin(async {
                Err(CallFailed::new(
                    "unreachable",
                    "the fake model could not be reached",
                ))
            });
        }
        let sentence = state["condition"].as_str().unwrap_or_default();
        let fact = FAKE_FACTS
            .iter()
            .find(|(f, _)| *f == sentence)
            .map(|(_, line)| *line);
        let mut answers = HashMap::new();
        for question in questions {
            let answer = match (question.id.as_str(), &question.kind) {
                ("evidence", Kind::Choose(options)) => {
                    let choice = fact
                        .and_then(|line| {
                            options
                                .iter()
                                .find(|(o, what)| o != "none" && what.contains(line))
                        })
                        .map_or("none".to_string(), |(o, _)| o.clone());
                    let probabilities = options
                        .iter()
                        .map(|(o, _)| (o.clone(), if *o == choice { 1.0 } else { 0.0 }))
                        .collect();
                    Answer::Chose {
                        choice,
                        probabilities,
                    }
                }
                ("met", Kind::YesNo) => {
                    let shown = fact.is_some_and(|line| state["lines"].to_string().contains(line));
                    Answer::Yes(if shown { 0.95 } else { 0.05 })
                }
                ("risky", Kind::YesNo) => {
                    let action = state["action"].as_str().unwrap_or_default();
                    let risky = FAKE_RISKY.iter().any(|r| action.contains(r));
                    Answer::Yes(if risky { 0.92 } else { 0.04 })
                }
                (id, Kind::YesNo) => {
                    let n: usize = id.trim_start_matches('c').parse().unwrap();
                    let sentence = state["conditions"][n].as_str().unwrap_or_default();
                    Answer::Yes(if FAKE_VAGUE.contains(&sentence) {
                        0.05
                    } else {
                        0.95
                    })
                }
                (id, _) => panic!("the fake has no answer for {id}"),
            };
            answers.insert(question.id.clone(), answer);
        }
        let meta = CallMeta {
            provider: "fake",
            model: "fake-1".into(),
            request_id: Some("fake".into()),
            latency_ms: 1,
            cost: None,
            exchange: None,
        };
        let slow = if answers.contains_key("risky") {
            self.slow.load(Ordering::SeqCst)
        } else {
            0
        };
        Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(slow)).await;
            Ok((answers, meta))
        })
    }
}

pub(super) struct Harness {
    pub(super) browsers: Arc<Browsers>,
    /// The session the harness acts in, and its id.
    pub(super) entry: Arc<host::Entry>,
    pub(super) id: String,
    state: std::path::PathBuf,
    requests: u32,
    /// Every audit record, for the checks on what it holds.
    pub(super) audits: Vec<Map<String, Value>>,
}

impl Harness {
    pub(super) async fn start(
        state: &std::path::Path,
        engine: &str,
        provider: Option<Arc<dyn Provider>>,
    ) -> Harness {
        Harness::start_with(state, engine, provider, Vec::new()).await
    }

    /// A harness whose local origins the site rules read as the HTTPS
    /// sites they stand in for, `(local, site)` each.
    pub(super) async fn start_with(
        state: &std::path::Path,
        engine: &str,
        provider: Option<Arc<dyn Provider>>,
        origins: Vec<(String, String)>,
    ) -> Harness {
        let vault = Vault::new(state.to_path_buf());
        let mut browsers = Browsers::new(state.to_path_buf(), vault, super::HEADLESS);
        browsers.test = Some((latch(), origins));
        // The owner's screen is there, for a handoff.
        browsers.screen = Some(true);
        browsers.provider = provider;
        let browsers = Arc::new(browsers);
        let (id, entry) = browsers.open(engine, CALLER, None).await.unwrap();
        Harness {
            browsers,
            entry,
            id,
            state: state.to_path_buf(),
            requests: 0,
            audits: Vec::new(),
        }
    }

    /// A command in the harness's session.
    pub(super) async fn call(&mut self, command: &str, args: Value) -> Value {
        let mut audit = Map::new();
        let (result, _) = super::host_call(
            &self.browsers,
            CALLER.into(),
            command,
            &self.id,
            &args,
            None,
            &mut audit,
        )
        .await
        .unwrap();
        result
    }

    /// Closes every session and browser, before the test's folder goes.
    pub(super) async fn close(self) {
        self.browsers.close_all().await;
    }

    /// Loads `url` in the harness's session, which keeps what earlier
    /// pages left (a cart, a sign-in).
    pub(super) async fn open(&mut self, url: &str) -> Value {
        self.call("goto", json!({"url": url})).await
    }

    pub(super) async fn text(&mut self) -> String {
        self.call("text", json!({})).await["text"]
            .as_str()
            .unwrap()
            .to_string()
    }

    /// A fresh request id, as the client makes one.
    fn request(&mut self) -> String {
        self.requests += 1;
        format!("r{:08x}{}", rand::random::<u32>(), self.requests)
    }

    /// One request to the executor, as the host passes it on.
    pub(super) async fn send(&mut self, mut args: Value) -> Result<Value, Failure> {
        if args["request"].is_null() {
            args["request"] = json!(self.request());
        }
        let mut audit = Map::new();
        let result = steps::run(
            &self.browsers,
            &self.entry,
            &self.id,
            CALLER,
            None,
            &args,
            &mut audit,
        )
        .await;
        self.audits.push(audit);
        result
    }

    /// `act`, as the client builds it.
    async fn act(
        &mut self,
        action: ActAction,
        words: &[&str],
        check: Option<&str>,
    ) -> Result<Value, Failure> {
        let word = |n: usize| words.get(n).map(|w| w.to_string());
        let literal = act_literal(Some(action), None, word(0), word(1)).unwrap();
        let plan = Plan {
            steps: vec![Step {
                literal: Some(literal),
                check: check.map(|c| Check::parse(c).unwrap()),
                ..Step::default()
            }],
            ..Plan::default()
        };
        self.send(json!({"plan": plan})).await
    }

    /// The plan command's short form, as the client compiles it.
    pub(super) async fn short(&mut self, steps: &[&str]) -> Result<Value, Failure> {
        let steps: Vec<String> = steps.iter().map(|s| s.to_string()).collect();
        let plan = do_plan(&steps, None, false, None, &[], &[]).unwrap();
        self.send(json!({"plan": plan})).await
    }

    /// `do --plan-file`, as the client reads it.
    async fn plan_file(&mut self, plan: Value) -> Result<Value, Failure> {
        let file = self.state.join("plan.json");
        std::fs::write(&file, plan.to_string()).unwrap();
        let plan = do_plan(&[], Some(&file), false, None, &[], &[]).unwrap();
        self.send(json!({"plan": plan})).await
    }

    pub(super) async fn confirm(&mut self, id: &str) -> Result<Value, Failure> {
        self.send(json!({"plan": {"steps": []}, "confirm": id}))
            .await
    }

    fn history(&self, request: &str) -> Value {
        steps::history(&self.browsers, &self.id, CALLER, request).unwrap()
    }
}

/// Keeps `value` as `name` in `LATCH_BROWSER_EVIDENCE`, when it is set.
fn evidence(name: &str, value: &Value) {
    if let Some(dir) = std::env::var_os("LATCH_BROWSER_EVIDENCE") {
        let text = serde_json::to_string_pretty(value).unwrap();
        std::fs::write(std::path::Path::new(&dir).join(name), text).unwrap();
    }
}

fn stopped(name: &str, failure: &Failure) {
    let shown = json!({"exit": u8::from(failure.exit), "code": failure.code,
                       "message": failure.message, "details": failure.details});
    evidence(name, &shown);
}

fn refused(failure: &Failure) -> (Exit, &str) {
    (failure.exit, failure.code.as_str())
}

fn confirmation(failure: &Failure) -> Value {
    failure.details["stop"]["confirmation"].clone()
}

async fn literal_steps_take_effect_and_their_checks_decide_the_outcome(engine: &str) {
    let (_dir, state, _area) = test_state(engine);
    let site = site();
    let fake: Arc<dyn Provider> = Arc::new(Fake::default());
    let mut h = Harness::start(&state, engine, Some(fake)).await;
    let page = h
        .open(&format!("{site}/expense-new.html?from=invoices"))
        .await;
    let merchant = find(&page, "textbox", "Merchant");
    let category = find(&page, "combobox", "Category");
    let amount = find(&page, "textbox", "Amount (USD)");
    let save = find(&page, "button", "Save draft");

    // type and select, each with a field check.
    let typed = h
        .act(
            ActAction::Type,
            &[&merchant, "Acme Hosting"],
            Some(&format!("field: {merchant}=Acme Hosting")),
        )
        .await
        .unwrap();
    evidence("b5-act-type.json", &typed);
    assert_eq!(typed["status"], "reached");
    assert_eq!(typed["steps"][0]["outcome"], "checked");
    assert_eq!(typed["trail"][0]["result"], "dispatched");
    assert_eq!(typed["trail"][0]["chosen"], "ref");
    assert!(
        typed["trail"][0]["risk_p"].is_null(),
        "typing needs no risk decision"
    );
    let selected = h
        .act(
            ActAction::Select,
            &[&category, "Office supplies"],
            Some(&format!("field: {category}=Office supplies")),
        )
        .await
        .unwrap();
    assert_eq!(selected["status"], "reached");
    let shown = h.call("observe", json!({})).await;
    let field = shown["elements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "Category")
        .unwrap();
    assert_eq!(field["value"], "Office supplies", "the option changed");

    // No check: only dispatched, never reached.
    let sent = h
        .act(ActAction::Type, &[&amount, "388.50"], None)
        .await
        .unwrap();
    assert_eq!(sent["status"], "dispatched");
    assert_eq!(sent["steps"][0]["outcome"], "dispatched");

    // URL checks by component, typed and plan-file routes alike.
    let held = h
        .act(ActAction::Scroll, &["down"], Some("query: from=invoices"))
        .await
        .unwrap();
    assert_eq!(held["steps"][0]["evidence"]["check"], "query");
    let failure = h
        .plan_file(json!({"within": 3, "steps": [{"literal": {"op": "scroll", "text": "up"},
                                                  "check": {"kind": "path", "value": "invoices"}}]}))
        .await
        .unwrap_err();
    stopped("b5-check-not-met.json", &failure);
    assert_eq!(refused(&failure), (Exit::Failed, "check_not_met"));
    assert_eq!(failure.details["status"], "stopped");
    assert_eq!(failure.details["steps"][0]["outcome"], "stopped");
    assert_eq!(failure.details["trail"][0]["result"], "dispatched");

    // A click gets a risk decision; "Save draft" is fine. `new text` must
    // not hold before, and must after.
    let saved = h
        .act(ActAction::Click, &[&save], Some("new text: Draft saved"))
        .await
        .unwrap();
    evidence("b5-act-click.json", &saved);
    assert_eq!(saved["status"], "reached");
    assert!(saved["trail"][0]["risk_p"].as_f64().unwrap() < 0.5);
    assert_eq!(saved["page"]["title"], "Expense · Expenses");
    let failure = h
        .act(ActAction::Scroll, &["down"], Some("new text: Draft saved"))
        .await
        .unwrap_err();
    assert_eq!(refused(&failure), (Exit::Failed, "check_not_met"));
    assert!(
        failure.details["trail"].as_array().unwrap().is_empty(),
        "nothing was sent"
    );

    // A sentence check, judged by the model: model_assessed, for review.
    let page = h.open(&format!("{site}/expense-new.html")).await;
    let save = find(&page, "button", "Save draft");
    let judged = h
        .short(&[&format!("click {save} -> the draft was saved")])
        .await
        .unwrap();
    assert_eq!(judged["status"], "reached");
    assert_eq!(judged["steps"][0]["outcome"], "model_assessed");
    assert!(
        judged["steps"][0]["evidence"]["evidence"]
            .as_str()
            .unwrap()
            .contains("Draft saved")
    );
    assert_eq!(judged["review"], true);
    let failure = h
        .short(&[&format!("click {save} -> finish the expense report")])
        .await
        .unwrap_err();
    assert_eq!(refused(&failure), (Exit::Usage, "vague_condition"));

    // A download the step starts, and the origin.
    let page = h.open(&format!("{site}/invoices.html")).await;
    let link = page["elements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "Download" && e["row"].as_str().unwrap_or("").contains("September"))
        .unwrap()["ref"]
        .as_str()
        .unwrap()
        .to_string();
    let downloaded = h
        .short(&[&format!("click {link} -> download: complete")])
        .await
        .unwrap();
    assert!(downloaded["steps"][0]["evidence"]["download"].is_string());
    let here = h
        .act(
            ActAction::Scroll,
            &["down"],
            Some(&format!("origin: {site}")),
        )
        .await
        .unwrap();
    assert_eq!(here["status"], "reached");

    // The audit: the request, each action's record, never typed text,
    // page text or what a check looked for.
    let audits = serde_json::to_string(&h.audits).unwrap();
    for never in [
        "Acme Hosting",
        "388.50",
        "Draft saved",
        "Office supplies",
        "from=invoices",
    ] {
        assert!(!audits.contains(never), "the audit holds {never:?}");
    }
    let click = h
        .audits
        .iter()
        .find(|a| a["step_records"][0]["op"] == "click")
        .unwrap();
    evidence("b5-audit-click.json", &Value::Object(click.clone()));
    let record = &click["step_records"][0];
    assert_eq!(record["chosen"], "ref");
    assert_eq!(record["dispatch"], "sent");
    assert!(record["risk_p"].is_number());
    assert!(audits.contains("text_sha256"));
    h.close().await;
}

async fn consequential_actions_stop_for_a_confirmation_bound_to_what_was_reviewed(engine: &str) {
    let (_dir, state, _area) = test_state(engine);
    let site = site();
    let slow = Arc::new(Fake::default());
    let fake: Arc<dyn Provider> = slow.clone();
    let mut h = Harness::start(&state, engine, Some(fake)).await;
    let page = h.open(&format!("{site}/order.html")).await;
    let place = find(&page, "button", "Place order");

    // "Place order" stops before anything is sent, with what was reviewed.
    let failure = h.act(ActAction::Click, &[&place], None).await.unwrap_err();
    stopped("b5-confirm-needed.json", &failure);
    assert_eq!(refused(&failure), (Exit::Denied, "confirm_needed"));
    let issued = confirmation(&failure);
    assert_eq!(issued["target"]["name"], "Place order");
    assert_eq!(issued["target"]["ref"], place);
    assert_eq!(issued["title"], "Checkout · Shop");
    assert!(issued["lines"].to_string().contains("Total $10.00"));
    assert!(issued["risk_p"].as_f64().unwrap() >= 0.5);
    assert!(failure.details["trail"].as_array().unwrap().is_empty());
    assert_eq!(failure.details["page"]["title"], "Checkout · Shop");
    // With a screenshot of the page, fetched by its handle.
    let handle = &issued["screenshot"];
    assert_eq!(handle["shot"], "final");
    assert_eq!(failure.details["screenshot"], *handle);
    let png = steps::shot(
        &h.browsers,
        &h.id,
        CALLER,
        handle["request"].as_str().unwrap(),
        "final",
    )
    .unwrap();
    assert!(png.starts_with(b"\x89PNG"));
    assert_eq!(
        steps::shot(
            &h.browsers,
            &h.id,
            "another-caller",
            handle["request"].as_str().unwrap(),
            "final"
        )
        .unwrap_err()
        .code,
        "no_such_request"
    );

    // A plain click on the same ref, or Space on it (which activates it as
    // a click does), is judged again: it stops with a new confirmation,
    // and names the one command that carries it out.
    for (action, words) in [
        (ActAction::Click, vec![place.as_str()]),
        (ActAction::Press, vec![place.as_str(), "Space"]),
    ] {
        let again = h.act(action, &words, None).await.unwrap_err();
        assert_eq!(refused(&again), (Exit::Denied, "confirm_needed"));
        assert_eq!(confirmation(&again)["target"]["name"], "Place order");
        assert_ne!(confirmation(&again)["id"], issued["id"]);
        assert!(again.message.contains("`latch browser do --confirm c"));
        assert!(!again.message.contains("act click"));
        let text = h.text().await;
        assert!(text.contains("Review your order"));
        assert!(!text.contains("Order placed"));
    }

    // `do --confirm C` carries it out, once.
    let placed = h.confirm(issued["id"].as_str().unwrap()).await.unwrap();
    evidence("b5-confirmed.json", &placed);
    assert_eq!(placed["steps"][0]["outcome"], "dispatched");
    assert_eq!(placed["steps"][0]["confirm"], issued["id"]);
    assert_eq!(placed["trail"][0]["confirmation"], issued["id"]);
    assert_eq!(placed["page"]["title"], "Order · Shop");
    let again = h.confirm(issued["id"].as_str().unwrap()).await.unwrap_err();
    assert_eq!(refused(&again), (Exit::Denied, "confirm_stale"));
    assert!(again.message.contains("already used"));
    let unknown = h.confirm("c000000000000").await.unwrap_err();
    assert_eq!(refused(&unknown), (Exit::Denied, "confirm_stale"));
    assert!(unknown.message.contains("does not exist"));

    // Repriced from $10 to $100 on the same page, same ref and name: stale.
    let page = h.open(&format!("{site}/order.html")).await;
    let place = find(&page, "button", "Place order");
    let upgrade = find(&page, "button", "Upgrade to express shipping");
    let failure = h.act(ActAction::Click, &[&place], None).await.unwrap_err();
    assert_eq!(failure.code, "confirm_needed");
    h.act(ActAction::Click, &[&upgrade], None).await.unwrap();
    let stale = h
        .confirm(confirmation(&failure)["id"].as_str().unwrap())
        .await
        .unwrap_err();
    stopped("b5-confirm-stale.json", &stale);
    assert_eq!(refused(&stale), (Exit::Denied, "confirm_stale"));
    // Nothing was sent: the answer reads no new snapshot, and says so.
    assert!(
        stale.details["page"]["note"].is_string(),
        "{}",
        stale.details
    );
    assert!(stale.details["page"].get("elements").is_none());
    let text = h.text().await;
    assert!(text.contains("$100.00") && !text.contains("Order placed"));

    // Issued on a page since replaced by a new load of it: stale.
    let page = h.open(&format!("{site}/order.html")).await;
    let place = find(&page, "button", "Place order");
    let issued = confirmation(&h.act(ActAction::Click, &[&place], None).await.unwrap_err());
    h.open(&format!("{site}/order.html")).await;
    let replaced = h.confirm(issued["id"].as_str().unwrap()).await.unwrap_err();
    assert_eq!(refused(&replaced), (Exit::Denied, "confirm_stale"));

    // A line break in typed text is an Enter: judged as the form's submit.
    let page = h.open(&format!("{site}/order.html")).await;
    let note = find(&page, "textbox", "Gift note");
    let failure = h
        .act(ActAction::Type, &[&note, "for Ada\n"], None)
        .await
        .unwrap_err();
    stopped("b5-type-newline.json", &failure);
    assert_eq!(refused(&failure), (Exit::Denied, "confirm_needed"));
    let issued = confirmation(&failure);
    assert_eq!(issued["acts_on"]["name"], "Place order");
    assert!(!h.text().await.contains("Order placed"));
    let placed = h.confirm(issued["id"].as_str().unwrap()).await.unwrap();
    assert_eq!(placed["trail"][0]["op"], "type");
    assert_eq!(placed["page"]["title"], "Order · Shop");

    // A total that changes while the model judges a click: not sent.
    let page = h.open(&format!("{site}/order.html")).await;
    let soon = find(&page, "button", "Reprice in a moment");
    let upgrade = find(&page, "button", "Upgrade to express shipping");
    h.act(ActAction::Click, &[&soon], None).await.unwrap();
    slow.slow.store(3000, Ordering::SeqCst);
    let failure = h
        .act(ActAction::Click, &[&upgrade], None)
        .await
        .unwrap_err();
    slow.slow.store(0, Ordering::SeqCst);
    stopped("b5-changed-while-judged.json", &failure);
    assert_eq!(refused(&failure), (Exit::Failed, "stale_snapshot"));
    assert_eq!(failure.details["trail"][0]["result"], "refused");
    assert!(
        failure.details["page"]["lines"]
            .to_string()
            .contains("$100.00")
    );

    // Enter in the checkout form's field: judged as its submit.
    let page = h.open(&format!("{site}/order.html")).await;
    let note = find(&page, "textbox", "Gift note");
    let failure = h
        .act(ActAction::Press, &[&note, "Enter"], None)
        .await
        .unwrap_err();
    stopped("b5-enter.json", &failure);
    assert_eq!(refused(&failure), (Exit::Denied, "confirm_needed"));
    let issued = confirmation(&failure);
    assert_eq!(issued["acts_on"]["name"], "Place order");
    let placed = h.confirm(issued["id"].as_str().unwrap()).await.unwrap();
    assert_eq!(placed["trail"][0]["op"], "press");
    assert_eq!(placed["page"]["title"], "Order · Shop");

    // Space confirmed is carried out as Space, not as a click.
    let page = h.open(&format!("{site}/order.html")).await;
    let place = find(&page, "button", "Place order");
    let failure = h
        .act(ActAction::Press, &[&place, "Space"], None)
        .await
        .unwrap_err();
    let issued = confirmation(&failure);
    let placed = h.confirm(issued["id"].as_str().unwrap()).await.unwrap();
    assert_eq!(placed["trail"][0]["op"], "press");
    assert_eq!(placed["page"]["title"], "Order · Shop");

    let audits = serde_json::to_string(&h.audits).unwrap();
    assert!(audits.contains("\"confirmation\""));
    assert!(!audits.contains("$10.00") && !audits.contains("Sharp Objects"));
    h.close().await;
}

async fn with_no_model_clicks_and_enter_stop_and_issued_confirmations_still_dispatch(engine: &str) {
    let (_dir, state, _area) = test_state(engine);
    let site = site();
    let fake = Arc::new(Fake::default());
    let provider: Arc<dyn Provider> = fake.clone();
    let mut h = Harness::start(&state, engine, Some(provider)).await;
    let page = h.open(&format!("{site}/order.html")).await;
    let place = find(&page, "button", "Place order");
    let note = find(&page, "textbox", "Gift note");
    let upgrade = find(&page, "button", "Upgrade to express shipping");
    let issued = confirmation(&h.act(ActAction::Click, &[&place], None).await.unwrap_err());

    fake.down.store(true, Ordering::SeqCst);
    for (action, words) in [
        (ActAction::Click, vec![upgrade.as_str()]),
        (ActAction::Press, vec![note.as_str(), "Enter"]),
    ] {
        let failure = h.act(action, &words, None).await.unwrap_err();
        stopped("b5-no-model.json", &failure);
        assert_eq!(refused(&failure), (Exit::Failed, "model_unavailable"));
        assert!(failure.details["trail"].as_array().unwrap().is_empty());
        // Why the model failed, in the audit and the journal as well.
        let audit = h.audits.last().unwrap();
        assert_eq!(audit["model_calls"][0]["outcome"], "failed");
        assert_eq!(audit["model_calls"][0]["reason"], "unreachable");
        assert_eq!(audit["stop"]["model_failed"], "unreachable");
        let read = h.history(audit["request"].as_str().unwrap());
        assert_eq!(read["stop"]["code"], "model_unavailable");
        assert_eq!(read["stop"]["model_failed"], "unreachable");
        assert_eq!(read["steps"][0]["model_failed"], "unreachable");
    }
    assert!(
        h.text().await.contains("$10.00"),
        "the upgrade was not clicked"
    );
    // Typing needs no model, nor does a typed check.
    let typed = h
        .act(
            ActAction::Type,
            &[&note, "for Ada"],
            Some(&format!("field: {note}=for Ada")),
        )
        .await
        .unwrap();
    assert_eq!(typed["status"], "reached");
    // A sentence check can't be judged: refused before anything runs.
    let failure = h
        .short(&[&format!("type {note} \"x\" -> the note was saved")])
        .await
        .unwrap_err();
    assert_eq!(failure.code, "model_unavailable");
    // The model vets the sentence, then fails judging it: the step's action
    // has gone out, and the answer says so.
    fake.down.store(false, Ordering::SeqCst);
    *fake.down_after.lock().unwrap() = Some(1);
    let failure = h
        .short(&[&format!("type {note} \"for Bo\" -> the draft was saved")])
        .await
        .unwrap_err();
    assert_eq!(failure.code, "model_unavailable");
    assert!(
        failure.message.contains("step 1's action was already sent"),
        "{}",
        failure.message
    );
    assert_eq!(failure.details["trail"][0]["op"], "type");
    // A confirmation issued before the outage still dispatches.
    let placed = h.confirm(issued["id"].as_str().unwrap()).await.unwrap();
    assert_eq!(placed["page"]["title"], "Order · Shop");

    // With no model at all (a host not logged in to Plow), the same.
    h.close().await;
    let mut bare = Harness::start(&state, engine, None).await;
    let page = bare.open(&format!("{site}/order.html")).await;
    let place = find(&page, "button", "Place order");
    let failure = bare
        .act(ActAction::Click, &[&place], None)
        .await
        .unwrap_err();
    assert_eq!(failure.code, "model_unavailable");
    assert!(
        failure.message.contains("not logged in to Plow"),
        "{}",
        failure.message
    );
    let audit = bare.audits.last().unwrap();
    assert_eq!(audit["stop"]["model_failed"], "no_model");
    assert!(audit.get("model_calls").is_none(), "no call was made");
    let read = bare.history(audit["request"].as_str().unwrap());
    assert_eq!(read["stop"]["model_failed"], "no_model");
    bare.close().await;
}

async fn a_key_on_an_element_that_cannot_take_the_focus_is_refused(engine: &str) {
    let (_dir, state, _area) = test_state(engine);
    let site = site();
    let fake: Arc<dyn Provider> = Arc::new(Fake::default());
    let mut h = Harness::start(&state, engine, Some(fake)).await;
    // The key would go to whatever had the focus before, here the search
    // field, and send its form.
    let page = h.open(&format!("{site}/disabled.html")).await;
    let field = find(&page, "textbox", "Search");
    let archive = find(&page, "button", "Archive");
    h.act(ActAction::Type, &[&field, "socks"], None)
        .await
        .unwrap();
    let failure = h
        .act(ActAction::Press, &[&archive, "Enter"], None)
        .await
        .unwrap_err();
    assert_eq!(
        refused(&failure),
        (Exit::Failed, "not_focused"),
        "{failure:?}"
    );
    assert!(
        h.text().await.contains("Search the shop"),
        "the form was not sent"
    );
    h.close().await;
}

async fn the_journal_outlives_a_host_restart_and_a_lost_reply_is_never_resent(engine: &str) {
    let (_dir, state, _area) = test_state(engine);
    let site = site();
    let fake: Arc<dyn Provider> = Arc::new(Fake::default());
    let mut h = Harness::start(&state, engine, Some(fake.clone())).await;
    let page = h.open(&format!("{site}/counter.html")).await;
    let slowly = find(&page, "button", "Add one slowly");

    // The host stops while the click is in flight.
    let args = json!({"request": "r-crash", "plan": do_plan(&[format!("click {slowly}")], None, false, None, &[], &[]).unwrap()});
    let (browsers, entry, crashed) = (h.browsers.clone(), h.entry.clone(), h.id.clone());
    let session = crashed.clone();
    let running = tokio::spawn(async move {
        let mut audit = Map::new();
        steps::run(&browsers, &entry, &session, CALLER, None, &args, &mut audit).await
    });
    let mut in_flight = Value::Null;
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        if let Ok(now) = steps::history(&h.browsers, &crashed, CALLER, "r-crash") {
            in_flight = now;
            if in_flight["actions"][0]["result"] == "unknown" {
                break;
            }
        }
    }
    assert_eq!(in_flight["status"], "running");
    assert_eq!(in_flight["actions"][0]["result"], "unknown");
    running.abort();
    drop(h);
    tokio::time::sleep(Duration::from_millis(500)).await;

    // A new host on the same state directory reads the journal, for the
    // crashed session's id.
    let mut h = Harness::start(&state, engine, Some(fake)).await;
    let after = steps::history(&h.browsers, &crashed, CALLER, "r-crash").unwrap();
    evidence("b5-history-after-restart.json", &after);
    assert_eq!(after["status"], "interrupted");
    assert_eq!(after["actions"][0]["result"], "unknown");
    assert_eq!(after["actions"][0]["op"], "click");
    assert_eq!(after["steps"][0]["outcome"], "not_run");

    // A reply that never arrived: the history answers, and the request id
    // can't be sent again.
    let page = h.open(&format!("{site}/counter.html")).await;
    let add = find(&page, "button", "Add one");
    let args = json!({"request": "r-lost", "plan": do_plan(&[format!("click {add}")], None, false, None, &[], &[]).unwrap()});
    let _lost = h.send(args.clone()).await;
    let read = h.history("r-lost");
    evidence("b5-history-lost-reply.json", &read);
    assert_eq!(read["status"], "dispatched");
    assert_eq!(read["actions"][0]["result"], "dispatched");
    assert_eq!(read["actions"][0]["target"]["name"], "Add one");
    let again = h.send(args).await.unwrap_err();
    assert_eq!(refused(&again), (Exit::Usage, "duplicate_request"));
    assert!(h.text().await.contains("Added: 1"), "one click, never two");
    assert_eq!(
        steps::history(&h.browsers, &h.id, "another-caller", "r-lost")
            .unwrap_err()
            .code,
        "no_such_request"
    );
    h.close().await;
}

/// The risk decision and a sentence check on the real decision model,
/// built only with `--features live-model` (`just gatekeeper`): asked on
/// Plow's decision route with the logged-in state's login.
#[cfg(feature = "live-model")]
#[tokio::test]
async fn live_the_model_asks_for_a_confirmation_only_where_one_is_needed() {
    let (_dir, state, _area) = test_state("cft");
    let provider = crate::model::host_provider(
        &crate::model::logged_in_for_tests(),
        crate::model::PLOW_MODEL,
    )
    .unwrap();
    let site = site();
    let mut h = Harness::start(&state, "cft", Some(Arc::from(provider))).await;
    let page = h.open(&format!("{site}/order.html")).await;
    let back = find(&page, "link", "Back to basket");
    let place = find(&page, "button", "Place order");
    let note = find(&page, "textbox", "Gift note");
    let failure = h.act(ActAction::Click, &[&place], None).await.unwrap_err();
    println!(
        "place order: {} {}",
        failure.code,
        confirmation(&failure)["risk_p"]
    );
    assert_eq!(failure.code, "confirm_needed");
    let failure = h
        .act(ActAction::Press, &[&note, "Enter"], None)
        .await
        .unwrap_err();
    println!(
        "enter: {} {}",
        failure.code,
        confirmation(&failure)["risk_p"]
    );
    assert_eq!(failure.code, "confirm_needed");
    let fine = h.act(ActAction::Click, &[&back], None).await.unwrap();
    println!("back to basket: risk {}", fine["trail"][0]["risk_p"]);
    assert_eq!(fine["page"]["title"], "Basket · Shop");
    let page = h.open(&format!("{site}/sign-in-prompt.html")).await;
    let close = find(&page, "button", "Close");
    let dismissed = h.act(ActAction::Click, &[&close], None).await;
    println!(
        "close the sign-in prompt: {:?}",
        dismissed
            .as_ref()
            .map(|d| &d["trail"][0]["risk_p"])
            .map_err(|f| (f.code.clone(), confirmation(f)["risk_p"].clone()))
    );
    assert_eq!(dismissed.unwrap()["status"], "dispatched");
    let page = h.open(&format!("{site}/expense-new.html")).await;
    let save = find(&page, "button", "Save draft");
    let saved = h
        .short(&[&format!("click {save} -> the draft was saved")])
        .await
        .unwrap();
    println!("save draft: {}", saved["steps"][0]);
    assert_eq!(saved["steps"][0]["outcome"], "model_assessed");
    let calls = serde_json::to_string(&h.audits).unwrap();
    assert!(calls.contains("\"provider\":\"plow\""));
    h.close().await;
}

/// Types a six-digit code on each of the test site's one-time-code pages,
/// then presses its button; every page but the focus trap must show the
/// code verified. Prints one line per page, as typing ended and what the
/// page then said.
async fn typing_a_code_reaches_the_fields_one_time_code_pages_use(engine: &str) {
    let (_dir, state, _area) = test_state(engine);
    let site = site();
    let fake: Arc<dyn Provider> = Arc::new(Fake::default());
    let mut h = Harness::start(&state, engine, Some(fake)).await;
    let pages = [
        ("otp-keys.html", "textbox", "Code", "Verify"),
        ("otp-split.html", "textbox", "Digit 1", "Verify"),
        ("otp-controlled.html", "textbox", "Code", "Verify"),
        ("otp-autofill.html", "textbox", "Code", "Verify"),
        (
            "otp-modal.html",
            "spinbutton",
            "Enter your 6-digit code",
            "Submit",
        ),
        (
            "otp-modal.html?trap",
            "spinbutton",
            "Enter your 6-digit code",
            "Submit",
        ),
    ];
    for (path, role, name, button) in pages {
        let page = h.open(&format!("{site}/{path}")).await;
        let field = find(&page, role, name);
        let typed = h.act(ActAction::Type, &[&field, "123456"], None).await;
        let typed = typed.map_or_else(|f| f.code, |t| t["status"].as_str().unwrap().to_string());
        let page = h.call("observe", json!({})).await;
        let go = find(&page, "button", button);
        let pressed = h.act(ActAction::Click, &[&go], None).await;
        let pressed =
            pressed.map_or_else(|f| f.code, |p| p["status"].as_str().unwrap().to_string());
        let text = h.text().await;
        println!("{engine} {path}: type {typed}, {button} {pressed}, page: {text:?}");
        assert_eq!(typed, "dispatched", "{path}");
        assert!(text.contains("verified 123456"), "{path}: {text}");
        if path == "otp-modal.html?trap" {
            // The field never holds the focus, so the code is set in it
            // without, and no select-all lands on the page instead.
            assert!(text.contains("input: unfocused"), "{path}: {text}");
            assert!(text.contains("selected: 0"), "{path}: {text}");
        }
    }
    // The code typed again from the first box, which already holds its
    // digit: the rest go to the boxes the focus moves on to.
    let page = h.open(&format!("{site}/otp-split.html")).await;
    let first = find(&page, "textbox", "Digit 1");
    h.act(ActAction::Type, &[&first, "1"], None).await.unwrap();
    h.act(ActAction::Type, &[&first, "123456"], None)
        .await
        .unwrap();
    // A field whose page turns every key away is not reported typed.
    let page = h.open(&format!("{site}/otp-keys.html")).await;
    let field = find(&page, "textbox", "Code");
    let failure = h
        .act(ActAction::Type, &[&field, "abcdef"], None)
        .await
        .unwrap_err();
    assert_eq!(
        refused(&failure),
        (Exit::Failed, "type_failed"),
        "{failure:?}"
    );
    h.close().await;
}

/// A return on the portal rebuilt from an agent's live run, each page's
/// steps in one plan on the refs the last answer gave. Its selection
/// cards draw a checkbox or radio with no name of its own, 0x0 in a span
/// that takes no clicks, and take the click on the whole card: each is
/// named by its card, and a click on it is the click on its card a person
/// makes.
async fn a_return_goes_through_selection_cards_in_batched_plans(engine: &str) {
    let (_dir, state, _area) = test_state(engine);
    let site = site();
    let fake: Arc<dyn Provider> = Arc::new(Fake::default());
    let mut h = Harness::start(&state, engine, Some(fake)).await;
    let page = h.open(&format!("{site}/return-portal.html")).await;
    macro_rules! plan {
        ($($step:expr),+ $(,)?) => {{
            let steps: Vec<String> = vec![$($step),+];
            let steps: Vec<&str> = steps.iter().map(String::as_str).collect();
            let answer = h.short(&steps).await.unwrap();
            assert_eq!(answer["status"], "dispatched", "{answer}");
            answer["page"].clone()
        }};
    }
    let page = plan!(
        format!(
            "type {} \"10001\"",
            find(&page, "textbox", "Email or Zip code")
        ),
        format!("type {} \"#1001\"", find(&page, "textbox", "Order Number")),
        format!("click {}", find(&page, "button", "Search")),
    );
    let page = plan!(format!("click {}", find(&page, "button", "Start a return")));
    let page = plan!(
        format!("click {}", find(&page, "checkbox", "Rib Tank, White / M")),
        format!("click {}", find(&page, "button", "Continue")),
    );
    let page = plan!(format!("click {}", find(&page, "button", "Add details")));
    let mut page = plan!(
        format!("click {}", find(&page, "radio", "Too big")),
        format!("click {}", find(&page, "button", "Continue")),
    );
    for text in ["Too loose", "Through the chest"] {
        page = plan!(
            format!(
                "type {} \"{text}\"",
                find(&page, "textbox", "Return reason")
            ),
            format!("click {}", find(&page, "button", "Continue")),
        );
    }
    let page = plan!(format!("click {}", find(&page, "button", "No thanks")));
    let _ = plan!(
        format!("click {}", find(&page, "radio", "Refund")),
        format!("click {}", find(&page, "button", "Continue")),
    );
    let text = h.text().await;
    assert!(text.contains("ready to submit: 1 items, Refund"), "{text}");
    h.close().await;
}

/// A click aimed at a control that lands on its own container does not
/// reach it when the container draws an ::after over it, or the control
/// is a button that takes no pointer events: the step is refused and
/// nothing is clicked. A checkbox that takes none is no exception under
/// such an ::after, unless the ::after takes no pointer events either:
/// then the click on it is the click on its card.
async fn a_container_drawn_over_its_target_still_covers_it(engine: &str) {
    let (_dir, state, _area) = test_state(engine);
    let site = site();
    let fake: Arc<dyn Provider> = Arc::new(Fake::default());
    let mut h = Harness::start(&state, engine, Some(fake)).await;
    let page = h.open(&format!("{site}/drawn-over.html")).await;
    for (role, name) in [
        ("button", "Save"),
        ("button", "Send"),
        ("checkbox", "Gift wrap"),
    ] {
        let target = find(&page, role, name);
        let failure = h.short(&[&format!("click {target}")]).await.unwrap_err();
        assert_eq!(
            refused(&failure),
            (Exit::Failed, "covered_target"),
            "{name}: {failure:?}"
        );
    }
    assert!(h.text().await.contains("nothing clicked"));
    let card = find(&page, "checkbox", "Gift box");
    let picked = h.short(&[&format!("click {card}")]).await.unwrap();
    assert_eq!(picked["status"], "dispatched", "{picked}");
    assert!(h.text().await.contains("card picked"));
    h.close().await;
}

/// The time a call spends waiting for the page is its `settle_ms`: a
/// `wait`'s polling, and a sentence check's turns between judgments, on a
/// total that changes two seconds after its click.
async fn waiting_for_the_page_counts_as_settle_time(engine: &str) {
    let (_dir, state, _area) = test_state(engine);
    let site = site();
    let mut h = Harness::start(&state, engine, Some(Arc::new(Fake::default()))).await;
    let page = h.open(&format!("{site}/order.html")).await;
    let soon = find(&page, "button", "Reprice in a moment");

    // The total changes two seconds after the click, however long the click
    // itself takes to come back: the wait is met no sooner than that.
    let clicking = std::time::Instant::now();
    h.act(ActAction::Click, &[&soon], None).await.unwrap();
    let waiting = std::time::Instant::now();
    let (waited, took) = host::timed(h.call("wait", json!({"text": "$100.00", "within": 5}))).await;
    let waited_ms = waiting.elapsed().as_millis() as u64;
    println!("wait: {waited} in {waited_ms} ms: {took:?}");
    assert_eq!(waited["met"], true);
    assert!(
        clicking.elapsed().as_millis() >= 2000,
        "{:?}",
        clicking.elapsed()
    );
    let settled = took.settle_ms.unwrap_or_default();
    assert!(settled + 300 >= waited_ms, "{settled} of {waited_ms}");

    let page = h.open(&format!("{site}/order.html")).await;
    let soon = find(&page, "button", "Reprice in a moment");
    let (checked, took) =
        host::timed(h.short(&[&format!("click {soon} -> the total is $100.00")])).await;
    println!("sentence check: {took:?}");
    assert_eq!(checked.unwrap()["steps"][0]["outcome"], "model_assessed");
    // Two seconds to the new total: at least one judgment came too soon,
    // and the second between it and the next was spent waiting.
    assert!(took.settle_ms.unwrap_or_default() >= 1000, "{took:?}");
    h.close().await;
}

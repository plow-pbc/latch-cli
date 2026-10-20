//! Step lists, targets by name and description, expectations, risky
//! clicks and request history, on the pinned engine through the host's own
//! `act`, `find` and `wait`, on the test site's expenses and shop pages.
//!
//! Without a decision model (no key in the state directory): exact names,
//! typed checks, the word list's review, and history. With one: a fake
//! provider answering from fixed facts about the test site, in every run;
//! and, built only with `--features live-model` (`just test-model`), the
//! same task on jev itself.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Map, Value, json};

use super::{CALLER, latch, test_state};
use crate::browser::client::read_steps;
use crate::browser::host::{self, Browsers};
use crate::browser::model::{Answer, Asked, CallMeta, Kind, Provider, Question};
use crate::browser::steps;
use crate::failure::{Exit, Failure};
use crate::vault::host::Vault;

/// The test site, plus the answers to its expense and order forms and a
/// page that takes three seconds.
fn site() -> String {
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
                        .replace("%24", "$")
                };
                let path = line.split(' ').nth(1).unwrap_or("/").to_string();
                let answer = |title: &str, text: String| {
                    format!("<!doctype html><meta charset=\"utf-8\"><title>{title}</title><h1>{title}</h1><p>{text}</p>")
                        .into_bytes()
                };
                let page = match path.as_str() {
                    "/expense-save" => {
                        let amount = field("amount");
                        let plain = !amount.is_empty()
                            && amount.chars().all(|c| c.is_ascii_digit() || c == '.');
                        Some(if !plain {
                            answer(
                                "New expense · Expenses",
                                "Amount must be a plain number, like 12.50. Your expense was not saved.".into(),
                            )
                        } else if field("go") == "submit" {
                            answer(
                                "Expense · Expenses",
                                format!("Expense submitted for approval. Amount: ${amount}"),
                            )
                        } else {
                            answer(
                                "Expense · Expenses",
                                format!("Draft saved. Amount: ${amount}"),
                            )
                        })
                    }
                    "/placed" => Some(answer("Order · Shop", "Order placed.".into())),
                    "/slow" => {
                        std::thread::sleep(Duration::from_secs(3));
                        Some(answer("Slow · Shop", "Slow page loaded.".into()))
                    }
                    file => std::fs::read(folder.join(file.trim_start_matches('/'))).ok(),
                };
                let mut socket = socket;
                let (status, page) = match page {
                    Some(page) => ("200 OK", page),
                    None => ("404 Not Found", Vec::new()),
                };
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    page.len()
                );
                let _ = socket.write_all(head.as_bytes());
                let _ = socket.write_all(&page);
            });
        }
    });
    format!("http://127.0.0.1:{port}")
}

struct Harness {
    browsers: Arc<Browsers>,
    /// The session the harness acts in, and its id.
    entry: Arc<host::Entry>,
    id: String,
    requests: u32,
    /// Every audit record, for the checks on what it holds.
    audits: Vec<Map<String, Value>>,
}

impl Harness {
    async fn start(state: &std::path::Path, provider: Option<Arc<dyn Provider>>) -> Harness {
        let vault = Vault::new(state.to_path_buf());
        let mut browsers = Browsers::new(state.to_path_buf(), vault);
        browsers.test = Some((latch(), Vec::new()));
        browsers.provider = provider;
        let browsers = Arc::new(browsers);
        let (id, entry) = browsers.open("default", CALLER, None).await.unwrap();
        Harness {
            browsers,
            entry,
            id,
            requests: 0,
            audits: Vec::new(),
        }
    }

    /// A new session on `url`, which takes the place of the one before.
    async fn open(&mut self, url: &str) -> Value {
        let mut audit = Map::new();
        let (opened, _) = host::call(
            &self.browsers,
            CALLER,
            "open",
            "default",
            &json!({"url": url}),
            None,
            &mut audit,
        )
        .await
        .unwrap();
        let id = opened["session"].as_str().unwrap().to_string();
        self.entry = self.browsers.session(&id, CALLER).await.unwrap();
        let before = std::mem::replace(&mut self.id, id);
        self.browsers.close(&before, CALLER).await.unwrap();
        opened
    }

    fn request(&mut self) -> String {
        self.requests += 1;
        format!("r{}", self.requests)
    }

    /// `act --steps`, as the client sends it: the lines parsed by the
    /// client's own reader.
    async fn steps(&mut self, lines: &str) -> (String, Result<Value, Failure>) {
        let request = self.request();
        let args = json!({"request": request, "list": true, "steps": read_steps(lines).unwrap()});
        (request.clone(), self.act(args).await)
    }

    /// One `act`.
    async fn act_one(&mut self, line: &str) -> Result<Value, Failure> {
        let request = self.request();
        let args = json!({"request": request, "list": false, "steps": read_steps(line).unwrap()});
        self.act(args).await
    }

    async fn act(&mut self, args: Value) -> Result<Value, Failure> {
        let mut audit = Map::new();
        let result = steps::act(&self.browsers, &self.entry, &args, &mut audit).await;
        self.audits.push(audit);
        result
    }

    async fn find(&mut self, args: Value) -> Result<Value, Failure> {
        let mut audit = Map::new();
        let result = steps::find(&self.browsers, &self.entry, &args, &mut audit).await;
        self.audits.push(audit);
        result
    }

    async fn wait(&mut self, args: Value) -> Result<Value, Failure> {
        let mut audit = Map::new();
        let result = steps::wait(&self.browsers, &self.entry, &args, &mut audit).await;
        self.audits.push(audit);
        result
    }

    fn history(&self, request: &str) -> Value {
        steps::history(&self.entry, request).unwrap()
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

fn row_ref(result: &Value, role: &str, name: &str, row: &str) -> String {
    result["elements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| {
            e["role"] == role && e["name"] == name && e["row"].as_str().unwrap_or("").contains(row)
        })
        .unwrap_or_else(|| panic!("no {role} {name:?} in row {row:?}"))["ref"]
        .as_str()
        .unwrap()
        .to_string()
}

fn dispatched(trace: &Value) -> Vec<String> {
    trace
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["dispatch"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[tokio::test]
async fn without_a_model_names_resolve_exactly_and_risky_clicks_need_a_ref() {
    let (_dir, state, _area) = test_state();
    let site = site();
    let mut h = Harness::start(&state, None).await;
    h.open(&format!("{site}/invoices.html")).await;

    // An exact name, no model call.
    let done = h.act_one(r#"click "New expense""#).await.unwrap();
    assert_eq!(done["resolved_by"], "name");
    assert_eq!(done["dispatch"], "dispatched");
    assert_eq!(done["target"]["name"], "New expense");
    assert_eq!(done["title"], "New expense · Expenses");
    assert!(done["elements"].as_array().unwrap().len() >= 6);

    // A description resolves by exact name only, and says why.
    let failure = h.act_one(r#"type "amount" 12.50"#).await.unwrap_err();
    assert_eq!(
        (failure.exit, failure.code.as_str()),
        (Exit::Failed, "no_such_target")
    );
    assert!(failure.message.contains("exactly"), "{}", failure.message);
    assert_eq!(failure.details["dispatch"], "not_sent");
    assert!(failure.details["elements"].is_array());

    // A sentence is refused before anything runs.
    let (request, result) = h
        .steps("type Merchant \"Acme Hosting\"\nclick \"Save draft\" --expect \"the draft was saved\"\n")
        .await;
    let failure = result.unwrap_err();
    assert_eq!(failure.code, "assist_off");
    assert!(
        steps::history(&h.entry, &request).is_err(),
        "nothing ran, so nothing was kept"
    );

    // Typed checks need no model: a whole form by exact names, in one list.
    let (request, result) = h
        .steps(
            "type Merchant \"Acme Hosting\" --expect-field e1=\"Acme Hosting\"\n\
             type \"Amount (USD)\" 388.50\n\
             select Category \"Office supplies\"\n\
             click \"Save draft\" --expect-text \"Draft saved\"\n",
        )
        .await;
    let done = result.unwrap();
    assert_eq!(done["status"], "done");
    assert_eq!(done["executed"], 4);
    assert_eq!(done["trace"][0]["expect"][0]["result"], "met");
    assert_eq!(
        done["trace"][3]["expect"][0],
        json!({"kind": "text", "result": "met"})
    );
    assert_eq!(h.history(&request)["status"], "done");

    // "Place order", on a page the agent hasn't seen, stops for a ref,
    // however it was named.
    h.open(&format!("{site}/basket.html")).await;
    let (request, result) = h
        .steps("click \"Continue to checkout\"\nclick \"Place order\"\n")
        .await;
    let failure = result.unwrap_err();
    assert_eq!(
        (failure.exit, failure.code.as_str()),
        (Exit::Denied, "confirm_target")
    );
    assert_eq!(
        dispatched(&failure.details["trace"]),
        ["dispatched", "not_sent"]
    );
    assert_eq!(failure.details["review"]["by"], "words");
    let target = failure.details["target"].clone();
    assert_eq!(target["name"], "Place order");
    assert_eq!(h.history(&request)["status"], "confirm_target");
    // Now seen, the click by ref goes through.
    let line = format!(
        "--snapshot {} click {} --expect-text \"Order placed\"",
        target["snapshot"].as_str().unwrap(),
        target["ref"].as_str().unwrap()
    );
    let done = h.act_one(&line).await.unwrap();
    assert_eq!(done["resolved_by"], "ref");
    assert_eq!(done["expect"][0]["result"], "met");

    // A position click names no element to review: on a page the agent
    // hasn't seen it is refused, never sent.
    h.open(&format!("{site}/basket.html")).await;
    let (_, result) = h
        .steps("click \"Continue to checkout\"\nclick --at 10,10\n")
        .await;
    let failure = result.unwrap_err();
    assert_eq!(
        (failure.exit, failure.code.as_str()),
        (Exit::Denied, "confirm_target")
    );
    assert_eq!(
        dispatched(&failure.details["trace"]),
        ["dispatched", "not_sent"]
    );
    // Shown now, it goes through.
    assert_eq!(
        h.act_one("click --at 10,10").await.unwrap()["dispatch"],
        "dispatched"
    );

    // find by role and exact name; wait for typed conditions.
    let invoices = h.open(&format!("{site}/invoices.html")).await;
    let found = h
        .find(json!({"role": "link", "name": "Download"}))
        .await
        .unwrap();
    assert_eq!(found["candidates"].as_array().unwrap().len(), 2);
    let found = h
        .find(json!({"description": "the unpaid invoice"}))
        .await
        .unwrap();
    assert_eq!(found["assist"], "off");
    let waited = h
        .wait(json!({"text": "Outstanding balance", "within": 2}))
        .await
        .unwrap();
    assert_eq!(waited["met"], true);
    let failure = h
        .wait(json!({"text": "never here", "within": 1}))
        .await
        .unwrap_err();
    assert_eq!(failure.code, "timeout");
    let failure = h
        .wait(json!({"for": "the invoices are listed", "within": 1}))
        .await
        .unwrap_err();
    assert_eq!(failure.code, "assist_off");

    // A field ref from a page that has gone is stale, not "not met".
    let ran = h.request();
    let steps = read_steps(r#"click "New expense""#).unwrap();
    h.act(json!({"request": ran, "list": false, "steps": steps}))
        .await
        .unwrap();
    let line = format!(
        "--snapshot {} scroll down --expect-field e1=x",
        invoices["snapshot"].as_str().unwrap()
    );
    let failure = h.act_one(&line).await.unwrap_err();
    assert_eq!(failure.code, "stale_snapshot");

    // A request id is never run twice.
    let again = json!({"request": ran, "list": false, "steps": read_steps("scroll down").unwrap()});
    assert_eq!(h.act(again).await.unwrap_err().code, "duplicate_request");

    // The audit: per-step records, never typed text or what was expected.
    let audits = serde_json::to_string(&h.audits).unwrap();
    for never in ["Acme Hosting", "388.50", "Draft saved", "Order placed"] {
        assert!(!audits.contains(never), "the audit holds {never:?}");
    }
    assert!(audits.contains("text_sha256"));
}

#[tokio::test]
async fn a_lost_reply_leaves_the_requests_history_to_read() {
    let (_dir, state, _area) = test_state();
    let site = site();
    let mut h = Harness::start(&state, None).await;
    h.open(&format!("{site}/basket.html")).await;
    let request = h.request();
    let args = json!({"request": request, "list": true, "steps": read_steps(
        "click \"Slow page\"\nscroll down --expect-text \"not on this page\" --within 3\nback\n"
    ).unwrap()});
    let (browsers, entry) = (h.browsers.clone(), h.entry.clone());
    let running = tokio::spawn(async move {
        let mut audit = Map::new();
        steps::act(&browsers, &entry, &args, &mut audit).await
    });
    // The reply is dropped: nobody waits for it. The history says how far
    // the list got while it still runs: a step whose action went out says
    // so while its expectation is still being waited for.
    let mut waiting_after_dispatch = false;
    for _ in 0..150 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let now = h.history(&request);
        if now["status"] != "running" {
            break;
        }
        let steps = dispatched(&now["trace"]);
        assert_eq!(steps[2], "not_sent");
        if steps[1] == "dispatched" {
            waiting_after_dispatch = true;
        }
    }
    assert!(
        waiting_after_dispatch,
        "step 2 read as dispatched while it waited"
    );
    drop(running);
    let mut finished = Value::Null;
    for _ in 0..100 {
        finished = h.history(&request);
        if finished["status"] != "running" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(finished["status"], "expect_not_met");
    assert_eq!(
        dispatched(&finished["trace"]),
        ["dispatched", "dispatched", "not_sent"]
    );
    assert_eq!(finished["trace"][0]["target"]["name"], "Slow page");
    assert_eq!(finished["trace"][1]["expect"][0]["result"], "not_met");
}

/// A decision model that answers from fixed facts about the test site, so
/// the model's paths run in every `just test`: which element each
/// description means (by a piece of its one-line description), which page
/// line proves each sentence, which sentences are vague, and which clicks
/// can't be undone.
struct Fake;

const FAKE_TARGETS: [(&str, &str); 5] = [
    ("amount", "\"Amount (USD)\""),
    ("date of the expense", "\"Date\""),
    ("the button that saves a draft", "\"Save draft\""),
    ("place the order", "\"Place order\""),
    ("the download for the unpaid invoice", "Unpaid"),
];
const FAKE_FACTS: [(&str, &str); 3] = [
    ("a form for a new expense is shown", "New expense"),
    ("the expense was saved as a draft", "Draft saved"),
    ("an outstanding balance is shown", "Outstanding balance"),
];
const FAKE_VAGUE: [&str; 1] = ["finish the expense report"];

/// The option whose description holds `want` (else "none"), with
/// probabilities that cover every option, sum to 1 and peak at it.
fn fake_pick(options: &[(String, String)], want: Option<&str>) -> Answer {
    let choice = want
        .and_then(|want| {
            options
                .iter()
                .find(|(o, what)| o != "none" && what.contains(want))
        })
        .map_or("none".to_string(), |(o, _)| o.clone());
    let probabilities = options
        .iter()
        .map(|(o, _)| {
            let p = if choice == "none" {
                if o == "none" { 1.0 } else { 0.0 }
            } else if *o == choice {
                0.95
            } else if o == "none" {
                0.05
            } else {
                0.0
            };
            (o.clone(), p)
        })
        .collect();
    Answer::Chose {
        choice,
        probabilities,
    }
}

impl Provider for Fake {
    fn ask<'a>(&'a self, state: &'a Value, questions: &'a [Question]) -> Asked<'a> {
        let sentence = state["condition"].as_str().unwrap_or_default();
        let fact = FAKE_FACTS
            .iter()
            .find(|(f, _)| *f == sentence)
            .map(|(_, line)| *line);
        let mut answers = HashMap::new();
        for question in questions {
            let answer = match (question.id.as_str(), &question.kind) {
                ("target", Kind::Choose(options)) => {
                    let said = state["agent_description"].as_str().unwrap_or_default();
                    let want = FAKE_TARGETS
                        .iter()
                        .find(|(d, _)| *d == said)
                        .map(|(_, w)| *w);
                    fake_pick(options, want)
                }
                ("evidence", Kind::Choose(options)) => fake_pick(options, fact),
                ("met", Kind::YesNo) => {
                    let shown = fact.is_some_and(|line| state["lines"].to_string().contains(line));
                    Answer::Yes(if shown { 0.97 } else { 0.03 })
                }
                ("risky", Kind::YesNo) => {
                    let element = state["element"].as_str().unwrap_or_default();
                    Answer::Yes(if element.contains("Place order") {
                        0.9
                    } else {
                        0.1
                    })
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
        };
        Box::pin(async move { Ok((answers, meta)) })
    }
}

#[tokio::test]
async fn a_list_with_descriptions_and_expectations_runs_a_task_in_one_request() {
    let (_dir, state, _area) = test_state();
    task_in_one_request(&state, Some(Arc::new(Fake)), "fake").await;
}

/// The same task on jev itself, built only with `--features live-model`
/// (`just test-model KEYFILE`): the key file is copied into the test's own
/// state directory.
#[cfg(feature = "live-model")]
#[tokio::test]
async fn live_jev_runs_the_task_in_one_request() {
    use std::os::unix::fs::PermissionsExt;
    let (_dir, state, _area) = test_state();
    let key = std::env::var_os("LATCH_TEST_JEV_KEY")
        .expect("LATCH_TEST_JEV_KEY names the jev key file for this test");
    let path = state.join(crate::browser::model::JEV_KEY_FILE);
    std::fs::copy(key, &path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    task_in_one_request(&state, None, "typesafe").await;
}

/// Seven actions in one request, a staged surprise, a vague sentence, a
/// risky click by description, `find` and `wait`, with `provider` (or the
/// host's own) as the decision model.
async fn task_in_one_request(
    state: &std::path::Path,
    provider: Option<Arc<dyn Provider>>,
    expected_provider: &str,
) {
    let site = site();
    let mut h = Harness::start(state, provider).await;
    let opened = h.open(&format!("{site}/invoices.html")).await;
    let september = row_ref(&opened, "link", "Download", "September");

    // Seven actions in one request, across a page load, with descriptions,
    // typed checks and sentences.
    let snapshot = opened["snapshot"].as_str().unwrap();
    let (request, result) = h
        .steps(&format!(
            "--snapshot {snapshot} click {september} --expect-download complete\n\
             click \"New expense\" --expect \"a form for a new expense is shown\"\n\
             type Merchant \"Acme Hosting\"\n\
             type \"amount\" 388.50\n\
             type \"date of the expense\" 2026-09-30\n\
             select Category \"Office supplies\"\n\
             click \"the button that saves a draft\" --expect-text \"Draft saved\" --expect \"the expense was saved as a draft\"\n"
        ))
        .await;
    let done = result.unwrap_or_else(|f| panic!("{} {} {:#}", f.code, f.message, f.details));
    evidence("b5-list.json", &done);
    evidence(
        "b5-list-audit.json",
        &Value::Object(h.audits.last().unwrap().clone()),
    );
    assert_eq!(done["status"], "done");
    assert_eq!(done["executed"], 7);
    let trace = &done["trace"];
    assert_eq!(trace[0]["expect"][0]["result"], "met");
    assert!(trace[0]["expect"][0]["download"].is_string());
    assert_eq!(trace[1]["resolved_by"], "name");
    assert_eq!(trace[1]["expect"][0]["kind"], "sentence");
    assert_eq!(trace[1]["expect"][0]["result"], "met");
    assert!(trace[1]["expect"][0]["evidence"].is_string());
    assert_eq!(trace[3]["resolved_by"], "model");
    assert_eq!(trace[3]["target"]["name"], "Amount (USD)");
    assert!(trace[3]["p"].as_f64().unwrap() >= 0.8);
    assert_eq!(trace[6]["expect"][1]["result"], "met");
    assert_eq!(h.history(&request)["status"], "done");
    let audit = h.audits.last().unwrap().clone();
    let calls = audit["model_calls"].as_array().unwrap();
    assert!(calls.iter().any(|c| c["purpose"] == "resolve"));
    assert!(calls.iter().any(|c| c["purpose"] == "judge"));
    assert!(calls.iter().all(|c| c["provider"] == expected_provider));
    assert!(
        calls
            .iter()
            .all(|c| c["latency_ms"].is_u64() && c["model_request"].is_string())
    );

    // A staged surprise: the site refuses the amount. The list stops at the
    // unmet expectation, with the trace and an element list.
    h.open(&format!("{site}/expense-new.html")).await;
    let (_, result) = h
        .steps(
            "type Merchant \"Acme Hosting\"\n\
             type \"amount\" \"$388.50\"\n\
             click \"Save draft\" --expect \"the expense was saved as a draft\"\n\
             click \"New expense\"\n",
        )
        .await;
    let failure = result.unwrap_err();
    stopped("b5-surprise.json", &failure);
    assert!(
        ["expect_not_met", "expect_unsure"].contains(&failure.code.as_str()),
        "{}",
        failure.code
    );
    assert_eq!(failure.details["step"], 3);
    assert_eq!(
        dispatched(&failure.details["trace"]),
        ["dispatched", "dispatched", "dispatched"]
    );
    assert!(failure.details["elements"].is_array());
    assert!(
        failure.details["lines"]
            .to_string()
            .contains("plain number")
    );

    // A vague expectation is refused before anything runs.
    let (_, result) = h
        .steps("type Merchant x\nclick \"Save draft\" --expect \"finish the expense report\"\n")
        .await;
    let failure = result.unwrap_err();
    stopped("b5-vague.json", &failure);
    assert_eq!(
        (failure.exit, failure.code.as_str()),
        (Exit::Usage, "vague_condition")
    );

    // "Place order" by description, on a page the agent hasn't seen.
    h.open(&format!("{site}/basket.html")).await;
    let (_, result) = h
        .steps("click \"Continue to checkout\"\nclick \"place the order\"\n")
        .await;
    let failure = result.unwrap_err();
    stopped("b5-confirm.json", &failure);
    assert_eq!(
        (failure.exit, failure.code.as_str()),
        (Exit::Denied, "confirm_target")
    );
    assert_eq!(failure.details["review"]["by"], "model");
    assert_eq!(failure.details["target"]["name"], "Place order");
    assert_eq!(failure.details["trace"][1]["resolved_by"], "model");

    // find by description, and wait for a sentence.
    h.open(&format!("{site}/invoices.html")).await;
    let found = h
        .find(json!({"description": "the download for the unpaid invoice"}))
        .await
        .unwrap();
    assert_eq!(found["resolved_by"], "model");
    assert!(
        found["candidates"][0]["row"]
            .as_str()
            .unwrap()
            .contains("Unpaid")
    );
    let waited = h
        .wait(json!({"for": "an outstanding balance is shown", "within": 5}))
        .await
        .unwrap();
    assert_eq!(waited["verdict"]["result"], "met");

    // Model calls are audited by kind of data, never content.
    let audits = serde_json::to_string(&h.audits).unwrap();
    for never in [
        "Acme Hosting",
        "the expense was saved",
        "place the order",
        "Outstanding",
    ] {
        assert!(!audits.contains(never), "the audit holds {never:?}");
    }
}

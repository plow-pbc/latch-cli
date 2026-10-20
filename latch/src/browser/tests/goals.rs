//! Instruction steps on Greenleaf Market, through the host's own `do`,
//! with a stand-in decision model told what to choose on each page: what
//! each pass offers, and each stop condition in turn. Whether a real model
//! chooses well is the plan suite's question, not these tests'.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use sha2::Digest;

use super::shop::serve_shop;
use super::steps::Harness;
use super::test_state;
use crate::browser::model::{Answer, Asked, CallMeta, Kind, Provider, Question};
use crate::browser::plan::{Plan, short_step};
use crate::failure::Failure;
use crate::vault::item::{CARD, Item, Raw};
use crate::vault::store::Backend;

/// What the stand-in model answers on one pass: the step's status, the
/// evidence line (an option whose text holds this), the operation, and
/// the target (an option whose text holds this) at probability `p`.
#[derive(Clone)]
struct Choice {
    status: &'static str,
    evidence: Option<&'static str>,
    operation: &'static str,
    target: Option<&'static str>,
    p: f64,
}

fn choose(operation: &'static str, target: Option<&'static str>) -> Choice {
    Choice {
        status: "not_yet",
        evidence: None,
        operation,
        target,
        p: 1.0,
    }
}

fn complete(evidence: &'static str, operation: &'static str) -> Choice {
    Choice {
        status: "complete",
        evidence: Some(evidence),
        operation,
        target: None,
        p: 1.0,
    }
}

type Mind = Box<dyn Fn(&Value) -> Choice + Send + Sync>;
/// One pass's questions: each id, with its options (id, text).
type Questions = Vec<(String, Vec<(String, String)>)>;

/// A decision model that answers each pass as `mind` says, given the
/// pass's state, and keeps each pass's questions. Clicks on "Place order"
/// and "Delete my account" can't be undone; nothing else can.
#[derive(Default)]
struct Told {
    mind: Mutex<Option<Mind>>,
    asked: Mutex<Vec<Questions>>,
}

impl Told {
    fn tell(&self, mind: impl Fn(&Value) -> Choice + Send + Sync + 'static) {
        *self.mind.lock().unwrap() = Some(Box::new(mind));
        self.asked.lock().unwrap().clear();
    }

    /// The options of question `id` on pass `n`.
    fn offered(&self, n: usize, id: &str) -> Vec<String> {
        self.asked.lock().unwrap()[n]
            .iter()
            .find(|(q, _)| q == id)
            .map(|(_, options)| options.iter().map(|(_, text)| text.clone()).collect())
            .unwrap_or_default()
    }

    fn passes(&self) -> usize {
        self.asked.lock().unwrap().len()
    }
}

fn certain(options: &[(String, String)], choice: &str, p: f64) -> Answer {
    let rest = if options.len() > 1 {
        (1.0 - p) / (options.len() - 1) as f64
    } else {
        0.0
    };
    Answer::Chose {
        choice: choice.to_string(),
        probabilities: options
            .iter()
            .map(|(o, _)| (o.clone(), if o == choice { p } else { rest }))
            .collect(),
    }
}

impl Provider for Told {
    fn ask<'a>(&'a self, state: &'a Value, questions: &'a [Question]) -> Asked<'a> {
        let mut answers = HashMap::new();
        if questions.iter().any(|q| q.id == "operation") {
            self.asked.lock().unwrap().push(
                questions
                    .iter()
                    .map(|q| {
                        let options = match &q.kind {
                            Kind::Choose(options) => options.clone(),
                            Kind::YesNo => Vec::new(),
                        };
                        (q.id.clone(), options)
                    })
                    .collect(),
            );
            let choice = (self
                .mind
                .lock()
                .unwrap()
                .as_ref()
                .expect("the model was told"))(state);
            for question in questions {
                let Kind::Choose(options) = &question.kind else {
                    continue;
                };
                let holding = |part: &str| {
                    options
                        .iter()
                        .find(|(o, text)| o != "none" && text.contains(part))
                        .map(|(o, _)| o.clone())
                };
                let (pick, p) = match question.id.as_str() {
                    "status" => (choice.status.to_string(), 1.0),
                    "evidence" => (
                        choice.evidence.and_then(holding).unwrap_or("none".into()),
                        1.0,
                    ),
                    "operation" => {
                        assert!(
                            options.iter().any(|(o, _)| o == choice.operation),
                            "{} is not offered among {options:?}",
                            choice.operation
                        );
                        (choice.operation.to_string(), 1.0)
                    }
                    "type" if choice.target.is_some_and(|t| t.starts_with("none, then ")) => {
                        // None, with that field's pair the next likeliest.
                        let then = choice.target.unwrap().trim_start_matches("none, then ");
                        let next = holding(then).expect("the field is offered");
                        let rest = 0.05 / (options.len().max(3) - 2) as f64;
                        let probabilities = options
                            .iter()
                            .map(|(o, _)| {
                                let p = match o {
                                    o if o == "none" => 0.8,
                                    o if *o == next => 0.15,
                                    _ => rest,
                                };
                                (o.clone(), p)
                            })
                            .collect();
                        answers.insert(
                            question.id.clone(),
                            Answer::Chose {
                                choice: "none".into(),
                                probabilities,
                            },
                        );
                        continue;
                    }
                    target => {
                        let named = choice.target.and_then(holding);
                        let pick = match (named, target) {
                            (Some(pick), _) => pick,
                            (None, "type") => "none".to_string(),
                            (None, _) => options[0].0.clone(),
                        };
                        (
                            pick,
                            if question.id == choice.operation {
                                choice.p
                            } else {
                                1.0
                            },
                        )
                    }
                };
                answers.insert(question.id.clone(), certain(options, &pick, p));
            }
        } else {
            for question in questions {
                let yes = match question.id.as_str() {
                    "risky" => {
                        let action = state["action"].as_str().unwrap_or_default();
                        if action.contains("Place order") || action.contains("Delete my account") {
                            0.95
                        } else {
                            0.03
                        }
                    }
                    _ => 0.95,
                };
                answers.insert(question.id.clone(), Answer::Yes(yes));
            }
        }
        let meta = CallMeta {
            provider: "told",
            model: "told-1".into(),
            request_id: None,
            latency_ms: 1,
        };
        Box::pin(async move { Ok((answers, meta)) })
    }
}

fn title(state: &Value) -> String {
    state["page"]["title"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn fields(state: &Value) -> String {
    state["page"]["fields"].to_string()
}

fn plan(steps: &[&str], site: &str, facts: &[(&str, &str)], items: &[String]) -> Value {
    let plan = Plan {
        steps: steps
            .iter()
            .map(|s| short_step(&s.replace("{site}", site)).unwrap())
            .collect(),
        facts: facts
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<BTreeMap<_, _>>(),
        items: items.to_vec(),
        ..Plan::default()
    };
    json!({"plan": plan})
}

/// A stopped plan's answer, and its stop code.
fn stop(result: Result<Value, Failure>) -> (String, Value) {
    let failure = result.expect_err("the plan should stop");
    (failure.code.clone(), failure.details)
}

fn sent(answer: &Value) -> usize {
    answer["trail"].as_array().map_or(0, |t| t.len())
}

#[tokio::test]
async fn instruction_steps_offer_only_what_is_possible_and_stop_in_order() {
    let (_dir, state, _area) = test_state();
    let site = serve_shop();
    let told = Arc::new(Told::default());
    let mut h = Harness::start(&state, Some(told.clone())).await;
    let at = |page: &str| format!("{site}/{page}");

    // A cookie modal: nothing behind it is offered until it is accepted.
    h.open(&at("reset.html")).await;
    h.open(&at("index.html?cookies=1")).await;
    told.tell(|s| {
        if title(s).starts_with("Drinks") {
            complete("[title] Drinks", "done")
        } else if s["page"]["on_screen"]
            .to_string()
            .contains("We use cookies")
        {
            choose("click", Some("Accept all cookies"))
        } else {
            choose("click", Some("link \"Drinks\""))
        }
    });
    let reached = h
        .send(plan(&["open the Drinks category"], &site, &[], &[]))
        .await
        .unwrap();
    assert_eq!(reached["status"], "reached", "{reached:#}");
    assert_eq!(reached["steps"][0]["outcome"], "model_assessed");
    assert_eq!(reached["review"], true);
    let first = told.offered(0, "click");
    assert!(first.iter().any(|o| o.contains("Accept all cookies")));
    assert!(
        !first.iter().any(|o| o.contains("link \"Drinks\"")),
        "{first:#?}"
    );
    assert!(!told.offered(0, "operation").contains(&"done".to_string()));
    assert!(
        told.offered(1, "click")
            .iter()
            .any(|o| o.contains("link \"Drinks\""))
    );
    assert_eq!(reached["trail"][0]["chosen"], "model");
    // Each action's screenshot is kept, and fetched by request and number.
    let request = reached["request"].as_str().unwrap();
    assert_eq!(
        reached["trail"][1]["screenshot"],
        json!({"request": request, "shot": 2})
    );
    let png = crate::browser::steps::shot(&h.browsers, &h.id, super::CALLER, request, "2").unwrap();
    assert!(png.starts_with(b"\x89PNG"));
    let audit = h.audits.last().unwrap();
    assert_eq!(
        audit["step_records"][1]["screenshot"]["sha256"],
        crate::state::to_hex(&sha2::Sha256::digest(&png))
    );

    // A dropdown already on the option asked for: held, with no action;
    // the option already chosen is offered, and marked.
    h.open(&at("reset.html")).await;
    h.open(&at("product.html?id=greek")).await;
    told.tell(|_| complete("combobox \"Size\": \"Regular\"", "stuck"));
    let held = h
        .send(plan(&["choose the Regular size"], &site, &[], &[]))
        .await
        .unwrap();
    assert_eq!(held["steps"][0]["outcome"], "already_held", "{held:#}");
    assert_eq!(sent(&held), 0);
    assert!(
        told.offered(0, "select")
            .iter()
            .any(|o| o.contains("\"Regular\" (already chosen)"))
    );

    // Page text alone never holds a step before it acts.
    told.tell(|_| complete("[screen] In stock", "stuck"));
    let (code, _) = stop(h.send(plan(&["add it to the cart"], &site, &[], &[])).await);
    assert_eq!(code, "already_true");

    // A click that changes nothing is not sent again.
    h.open(&at("deals.html")).await;
    told.tell(|_| choose("click", Some("Show more deals")));
    let (code, details) = stop(h.send(plan(&["show more deals"], &site, &[], &[])).await);
    assert_eq!(code, "repeat", "{details:#}");
    assert_eq!(sent(&details), 1);

    // A same-URL wizard: the same "Next" on a changed page is progress.
    h.open(&at("reset.html")).await;
    h.open(&at("wizard.html")).await;
    told.tell(|_| choose("click", Some("button \"Next\"")));
    let wizard = h
        .send(plan(
            &["go through the setup -> text: All set"],
            &site,
            &[],
            &[],
        ))
        .await
        .unwrap();
    assert_eq!(wizard["steps"][0]["outcome"], "checked", "{wizard:#}");
    assert_eq!(sent(&wizard), 3);

    // A note was asked for, and only a street address supplied.
    h.open(&at("driver-note.html")).await;
    told.tell(|_| choose("type", Some("none, then Note for the driver")));
    let (code, details) = stop(
        h.send(plan(
            &["add a note for the driver"],
            &site,
            &[("street", "12 Elm Road")],
            &[],
        ))
        .await,
    );
    assert_eq!(code, "needs_text", "{details:#}");
    assert_eq!(details["stop"]["field"]["name"], "Note for the driver");
    assert!(details["stop"]["field"]["ref"].is_string());
    assert_eq!(sent(&details), 0);
    assert!(
        told.offered(0, "type")
            .iter()
            .any(|o| o.contains("\"12 Elm Road\" (street)"))
    );

    // A value is typed as written, ` -> ` and all.
    told.tell(|s| {
        if fields(s).contains("A -> B") {
            complete(
                "[field] textbox \"Note for the driver\": \"A -> B\"",
                "done",
            )
        } else {
            choose("type", Some("textbox \"Note for the driver\""))
        }
    });
    let typed = h
        .send(plan(
            &["add the note \"A -> B\" for the driver"],
            &site,
            &[],
            &[],
        ))
        .await
        .unwrap();
    assert_eq!(typed["status"], "reached", "{typed:#}");
    let note = h
        .call(
            "eval",
            json!({"script": "document.getElementById('note').value"}),
        )
        .await;
    assert_eq!(note["value"], "A -> B");

    // A target no option stands out for: its likeliest candidates, as refs
    // in the page the answer shows.
    h.open(&at("reset.html")).await;
    h.open(&at("search.html?q=oat+milk")).await;
    told.tell(|_| Choice {
        p: 0.4,
        ..choose("click", Some("Add to cart"))
    });
    let (code, details) = stop(h.send(plan(&["add the oat milk"], &site, &[], &[])).await);
    assert_eq!(code, "ambiguous_target");
    let candidates = details["stop"]["candidates"].as_array().unwrap();
    assert!(!candidates.is_empty());
    for candidate in candidates {
        let at = candidate["ref"].as_str().unwrap();
        let shown = details["page"]["elements"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["ref"] == at)
            .unwrap();
        assert_eq!(shown["name"], candidate["name"]);
    }

    // Only a supplied URL is offered to navigate.
    told.tell(|s| {
        if title(s).starts_with("Weekly deals") {
            complete("[title] Weekly deals", "done")
        } else {
            choose("navigate", Some("deals.html"))
        }
    });
    let went = h
        .send(plan(&["open \"{site}/deals.html\""], &site, &[], &[]))
        .await
        .unwrap();
    assert_eq!(went["status"], "reached", "{went:#}");
    assert_eq!(went["trail"][0]["op"], "navigate");
    told.tell(|_| choose("stuck", None));
    let _ = stop(
        h.send(plan(
            &["open the page at {site}/deals.html"],
            &site,
            &[],
            &[],
        ))
        .await,
    );
    assert!(
        !told
            .offered(0, "operation")
            .contains(&"navigate".to_string())
    );

    // An out-of-stock item can't be added.
    h.open(&at("product.html?id=kale")).await;
    told.tell(|_| Choice {
        status: "cannot",
        ..choose("stuck", None)
    });
    let (code, _) = stop(h.send(plan(&["add it to the cart"], &site, &[], &[])).await);
    assert_eq!(code, "blocked");

    // A sign-in form, a CAPTCHA, and a card form stop for the agent.
    h.open(&at("reset.html?cart=greek:Regular")).await;
    h.open(&at("login.html")).await;
    told.tell(|_| choose("login", None));
    let (code, _) = stop(
        h.send(plan(&["proceed to checkout"], &site, &[], &[]))
            .await,
    );
    assert_eq!(code, "needs_login");
    h.open(&at("recaptcha/check.html")).await;
    let asked = told.passes();
    let (code, _) = stop(
        h.send(plan(&["proceed to checkout"], &site, &[], &[]))
            .await,
    );
    assert_eq!(code, "needs_owner");
    assert_eq!(
        told.passes(),
        asked,
        "a CAPTCHA stops before the model is asked"
    );
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
        .unwrap();
    h.open(&at(
        "reset.html?user=testuser@example.com&cart=greek:Regular",
    ))
    .await;
    h.open(&at("payment.html")).await;
    told.tell(|_| choose("fill", None));
    let (code, details) = stop(
        h.send(plan(
            &["pay with my saved card"],
            &site,
            &[],
            std::slice::from_ref(&card.item),
        ))
        .await,
    );
    assert_eq!(code, "needs_fields", "{details:#}");
    assert_eq!(details["stop"]["items"][0]["item"], card.item);
    let names: Vec<&str> = details["stop"]["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    for name in ["Name on card", "Card number", "Expiry", "CVV", "Pay"] {
        assert!(names.iter().any(|n| n.starts_with(name)), "{names:?}");
    }
    assert!(
        details["stop"]["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["ref"].is_string())
    );

    // An order is placed only once the agent confirms it.
    h.open(&at("checkout.html")).await;
    told.tell(|_| choose("click", Some("Place order")));
    let (code, details) = stop(h.send(plan(&["place the order"], &site, &[], &[])).await);
    assert_eq!(code, "confirm_needed", "{details:#}");
    let id = details["stop"]["confirmation"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let placed = h.confirm(&id).await.unwrap();
    assert_eq!(placed["status"], "dispatched", "{placed:#}");
    assert!(
        placed["page"]["title"]
            .as_str()
            .unwrap()
            .starts_with("Order placed")
    );
}

#[tokio::test]
async fn with_no_model_an_instruction_step_sends_nothing() {
    let (_dir, state, _area) = test_state();
    let site = serve_shop();
    let mut h = Harness::start(&state, None).await;
    h.open(&format!("{site}/index.html")).await;
    let failure = h
        .send(plan(&["open the Drinks category"], &site, &[], &[]))
        .await
        .unwrap_err();
    assert_eq!(failure.code, "model_unavailable", "{}", failure.message);
}

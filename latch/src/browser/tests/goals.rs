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
use crate::browser::plan::{Plan, short_step};
use crate::failure::Failure;
use crate::model::{Answer, Asked, CallMeta, Kind, Provider, Question};
use crate::vault::item::{CARD, Item, LOGIN, Raw};
use crate::vault::store::Backend;

conformance!(with_no_model_an_instruction_step_sends_nothing);

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
    /// Each pass's request size, and its state.
    sizes: Mutex<Vec<usize>>,
    states: Mutex<Vec<Value>>,
}

impl Told {
    fn tell(&self, mind: impl Fn(&Value) -> Choice + Send + Sync + 'static) {
        *self.mind.lock().unwrap() = Some(Box::new(mind));
        self.asked.lock().unwrap().clear();
        self.sizes.lock().unwrap().clear();
        self.states.lock().unwrap().clear();
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
            self.sizes
                .lock()
                .unwrap()
                .push(crate::model::request_size(state, questions));
            self.states.lock().unwrap().push(state.clone());
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
                        // `none` answers the fill question alone.
                        let named = match (choice.target, target) {
                            (Some("none"), "fill") => Some("none".to_string()),
                            (Some("none"), _) => None,
                            (named, _) => named.and_then(holding),
                        };
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
                        let near = state["lines_near"].to_string();
                        if action.contains("Place order")
                            || action.contains("Delete my account")
                            || near.contains("places your order")
                        {
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
            cost: None,
            exchange: None,
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
    let (_dir, state, _area) = test_state("cft");
    let site = serve_shop();
    let told = Arc::new(Told::default());
    let mut h = Harness::start_with(
        &state,
        "cft",
        Some(told.clone()),
        vec![(site.clone(), "https://shop.example".into())],
    )
    .await;
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
    assert_eq!(audit["goals"], 1);
    assert_eq!(
        audit["step_outcomes"],
        json!([{"step": 1, "kind": "goal", "outcome": "model_assessed", "actions": 2}])
    );
    assert_eq!(audit["model_calls"][0]["purpose"], "decide");
    // Each click gets its own risk decision, on what the review found.
    let purposes: Vec<&str> = audit["model_calls"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["purpose"].as_str())
        .collect();
    assert_eq!(
        purposes.iter().filter(|p| **p == "risk").count(),
        2,
        "{purposes:?}"
    );
    assert_eq!(
        audit["step_records"][1]["screenshot"]["sha256"],
        crate::state::to_hex(&sha2::Sha256::digest(&png))
    );

    // A page too big for one request is cut to fit Plow's decision route,
    // and the action says what was left out.
    h.open(&at("big.html")).await;
    told.tell(|s| {
        if title(s).starts_with("Greek Salad") {
            complete("[title] Greek Salad", "done")
        } else {
            choose("click", Some("link \"Greenleaf pantry item number 1, "))
        }
    });
    let big = h
        .send(plan(&["open the first pantry item"], &site, &[], &[]))
        .await
        .unwrap();
    assert_eq!(big["status"], "reached", "{big:#}");
    let first = told.sizes.lock().unwrap()[0];
    assert!(first <= crate::model::REQUEST_BUDGET, "{first}");
    assert!(
        big["trail"][0]["left_out"]["elements"].as_u64().unwrap() > 0,
        "{big:#}"
    );
    assert!(told.offered(0, "click").len() < 200);

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

    // A click that changes nothing is not sent again: with a typed check,
    // on the same page state; without one, never in the same step.
    for step in [
        "show more deals -> text: Rolled Oats 1kg",
        "show more deals",
    ] {
        h.open(&at("reset.html")).await;
        h.open(&at("deals.html")).await;
        told.tell(|_| choose("click", Some("Show more deals")));
        let (code, details) = stop(h.send(plan(&[step], &site, &[], &[])).await);
        assert_eq!(code, "repeat", "{step}: {details:#}");
        assert_eq!(sent(&details), 1, "{step}");
        assert!(
            !h.text().await.contains("Rolled Oats"),
            "{step}: the click did nothing"
        );
    }

    // Without a typed check, a control that changed the page is still not
    // clicked again in the step: one more Greek Salad is one. A message
    // the click writes again, even with the same words, reads as new.
    h.open(&at("reset.html")).await;
    h.open(&at("category.html?c=salads")).await;
    told.tell(|_| choose("click", Some("button \"Add to cart\" — in: Greek Salad")));
    let (code, details) = stop(
        h.send(plan(
            &[
                "add the Greek Salad to the cart",
                "add one more Greek Salad to the cart",
            ],
            &site,
            &[],
            &[],
        ))
        .await,
    );
    assert_eq!(
        (code.as_str(), details["stop"]["step"].as_u64()),
        ("repeat", Some(1))
    );
    let cart = h
        .call("eval", json!({"script": "localStorage.getItem('cart')"}))
        .await;
    assert!(
        cart["value"].as_str().unwrap().contains("\"qty\":1"),
        "{cart}"
    );
    told.tell(|s| {
        let added = s["page"]["new_since_the_step_began"]
            .to_string()
            .contains("Added to cart");
        if s["current_step"].as_str().unwrap().contains("one more") && added {
            complete("[screen] Added to cart", "done")
        } else if s["current_step"].as_str().unwrap().contains("one more")
            || s["actions_in_this_step"].as_array().unwrap().is_empty()
        {
            choose("click", Some("button \"Add to cart\" — in: Greek Salad"))
        } else {
            complete("[screen] Added to cart", "done")
        }
    });
    h.open(&at("reset.html")).await;
    h.open(&at("category.html?c=salads")).await;
    let two = h
        .send(plan(
            &[
                "add the Greek Salad to the cart",
                "add one more Greek Salad to the cart",
            ],
            &site,
            &[],
            &[],
        ))
        .await
        .unwrap();
    assert_eq!(two["status"], "reached", "{two:#}");
    assert_eq!(sent(&two), 2);
    let states = told.states.lock().unwrap().clone();
    // Step 2's first pass: the message step 1 left is old; after its click,
    // the same words, written again, are new.
    let step_two: Vec<&Value> = states
        .iter()
        .filter(|s| s["current_step"].as_str().unwrap().contains("one more"))
        .collect();
    let new = |s: &Value| s["page"]["new_since_the_step_began"].to_string();
    assert!(
        !new(step_two[0]).contains("Added to cart"),
        "{}",
        step_two[0]
    );
    assert!(
        new(step_two[1]).contains("Added to cart"),
        "{}",
        step_two[1]
    );

    // A typed check is waited for after the step's last action: what a
    // click brings two seconds later still completes the step.
    h.open(&at("reset.html")).await;
    h.open(&at("slow.html")).await;
    told.tell(|s| {
        if s["actions_in_this_step"].as_array().unwrap().is_empty() {
            choose("click", Some("Show more deals"))
        } else {
            choose("stuck", None)
        }
    });
    let slow = h
        .send(plan(
            &["show more deals -> text: Rolled Oats 1kg"],
            &site,
            &[],
            &[],
        ))
        .await
        .unwrap();
    assert_eq!(slow["steps"][0]["outcome"], "checked", "{slow:#}");
    // And one the model calls complete that never holds is not met.
    h.open(&at("deals.html")).await;
    told.tell(|s| {
        if s["actions_in_this_step"].as_array().unwrap().is_empty() {
            choose("click", Some("Show more deals"))
        } else {
            complete("[action] clicked button", "stuck")
        }
    });
    let (code, details) = stop(
        h.send(plan(
            &["show more deals -> text: Rolled Oats 1kg"],
            &site,
            &[],
            &[],
        ))
        .await,
    );
    assert_eq!(code, "check_not_met", "{details:#}");

    // A typed check is polled after each action, before the next decision:
    // a result that arrives a second after the click ends the step there,
    // with no second decision and no second click.
    h.open(&at("reset.html")).await;
    h.open(&at("slow.html?delay=1000")).await;
    told.tell(|_| choose("click", Some("Show more deals")));
    let polled = h
        .send(plan(
            &["show more deals -> text: Rolled Oats 1kg"],
            &site,
            &[],
            &[],
        ))
        .await
        .unwrap();
    assert_eq!(polled["steps"][0]["outcome"], "checked", "{polled:#}");
    assert_eq!(sent(&polled), 1);
    assert_eq!(told.passes(), 1, "one decision");

    // Two forms with a "Send" button each: Enter in the second form's
    // field submits the second form, not the first, so it is not a
    // second try of the first's button.
    h.open(&at("two-forms.html")).await;
    told.tell(
        |s| match s["actions_in_this_step"].as_array().unwrap().len() {
            0 => choose("click", Some("button \"Send\"")),
            1 => choose("type", Some("Email for offers")),
            2 => choose("press_enter", None),
            _ => choose("stuck", None),
        },
    );
    let (code, details) = stop(
        h.send(plan(
            &["send \"ada@example.com\" for offers"],
            &site,
            &[],
            &[],
        ))
        .await,
    );
    assert_eq!(code, "stuck", "{details:#}");
    assert_eq!(sent(&details), 3, "Enter in the second form was sent");

    // A control is one control whether it is clicked or submitted with
    // Enter: after "Send message", Enter in its form is a second try.
    h.open(&at("contact.html")).await;
    told.tell(
        |s| match s["actions_in_this_step"].as_array().unwrap().len() {
            0 => choose("click", Some("button \"Send message\"")),
            1 => choose("type", Some("textbox \"Your name\"")),
            _ => choose("press_enter", None),
        },
    );
    let (code, details) = stop(
        h.send(plan(&["send the message as \"Ada\""], &site, &[], &[]))
            .await,
    );
    assert_eq!(code, "repeat", "{details:#}");
    assert_eq!(sent(&details), 2);

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
    told.tell(|_| choose("type", Some("textbox \"Note for the driver\"")));
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
    // A step types only its own values: a fact it does not name is not one.
    assert!(told.offered(0, "type").is_empty());
    assert!(
        !told.states.lock().unwrap()[0]
            .to_string()
            .contains("12 Elm Road")
    );
    // A fact it names as {NAME} is.
    told.tell(|s| {
        if fields(s).contains("12 Elm Road") {
            complete(
                "[field] textbox \"Note for the driver\": \"12 Elm Road\"",
                "done",
            )
        } else {
            choose("type", Some("textbox \"Note for the driver\""))
        }
    });
    let named = h
        .send(plan(
            &["write {street} as the note for the driver"],
            &site,
            &[("street", "12 Elm Road")],
            &[],
        ))
        .await
        .unwrap();
    assert_eq!(named["status"], "reached", "{named:#}");
    assert!(
        told.offered(0, "type")
            .iter()
            .any(|o| o.contains("\"12 Elm Road\" (the plan's fact \"street\")"))
    );
    h.open(&at("driver-note.html")).await;

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

    // A CAPTCHA, and a card form with no suggestion chosen, stop for the
    // agent.
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
    // CAPTCHA frames a page loads but does not show are no stop; one shown
    // over the page is.
    h.open(&at("captcha-frames.html")).await;
    told.tell(|_| Choice {
        status: "cannot",
        ..choose("stuck", None)
    });
    let (code, _) = stop(h.send(plan(&["pay"], &site, &[], &[])).await);
    assert_eq!(code, "blocked", "hidden CAPTCHA frames are no wall");
    h.open(&at("captcha-frames.html?shown")).await;
    let asked = told.passes();
    let (code, _) = stop(h.send(plan(&["pay"], &site, &[], &[])).await);
    assert_eq!(code, "needs_owner");
    assert_eq!(
        told.passes(),
        asked,
        "a shown CAPTCHA stops before the model is asked"
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
    told.tell(|_| choose("fill", Some("none")));
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
    let audit = h.audits.last().unwrap();
    let purposes: Vec<&str> = audit["model_calls"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["purpose"].as_str())
        .collect();
    assert_eq!(purposes, ["decide", "risk"]);
    let refused = &audit["step_records"].as_array().unwrap()[0];
    assert_eq!(refused["risk_p"], json!(0.95));
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

    // `--output F`: the page as the plan ended, or stopped, is kept as the
    // request's `final` shot, which the client then fetches into F.
    told.tell(|s| {
        if title(s).starts_with("Drinks") {
            complete("[title] Drinks", "done")
        } else {
            choose("click", Some("link \"Drinks\""))
        }
    });
    let mut args = plan(&["open the Drinks category"], &site, &[], &[]);
    args["request"] = json!("r-output-1");
    args["output"] = json!(true);
    let mut audit = serde_json::Map::new();
    let (result, _) = super::host_call(
        &h.browsers,
        super::CALLER.into(),
        "do",
        &h.id,
        &args,
        None,
        &mut audit,
    )
    .await
    .unwrap();
    assert_eq!(
        result["screenshot"],
        json!({"request": "r-output-1", "shot": "final"})
    );
    let png = crate::browser::steps::shot(&h.browsers, &h.id, super::CALLER, "r-output-1", "final")
        .unwrap();
    assert!(png.starts_with(b"\x89PNG"));
    told.tell(|_| choose("stuck", None));
    args["request"] = json!("r-output-2");
    let failure = super::host_call(
        &h.browsers,
        super::CALLER.into(),
        "do",
        &h.id,
        &args,
        None,
        &mut audit,
    )
    .await
    .unwrap_err();
    assert_eq!(failure.code, "stuck");
    assert_eq!(
        failure.details["screenshot"],
        json!({"request": "r-output-2", "shot": "final"})
    );
    let kept =
        crate::browser::steps::shot(&h.browsers, &h.id, super::CALLER, "r-output-2", "final")
            .unwrap();
    assert!(kept.starts_with(b"\x89PNG"));
    h.close().await;
}

async fn with_no_model_an_instruction_step_sends_nothing(engine: &str) {
    let (_dir, state, _area) = test_state(engine);
    let site = serve_shop();
    let mut h = Harness::start(&state, engine, None).await;
    h.open(&format!("{site}/index.html")).await;
    let failure = h
        .send(plan(&["open the Drinks category"], &site, &[], &[]))
        .await
        .unwrap_err();
    assert_eq!(failure.code, "model_unavailable", "{}", failure.message);
    h.close().await;
}

/// Fills inside plans: the model chooses one of latch's suggestions for an
/// item the plan names, or `none`, and never maps a field; a sign-in, a
/// code and a passkey stop for the agent where the plan can't go on.
#[tokio::test]
async fn fills_inside_plans_take_latchs_suggestions_or_stop() {
    const PASSWORD: &str = "greenleaf-made-up-41";
    const KEY: &str = "JBSWY3DPEHPK3PXP";
    const NUMBER: &str = "4242424242424242";
    let (_dir, state, _area) = test_state("cft");
    let site = serve_shop();
    let told = Arc::new(Told::default());
    let mut h = Harness::start_with(
        &state,
        "cft",
        Some(told.clone()),
        vec![(site.clone(), "https://shop.example".into())],
    )
    .await;
    let at = |page: &str| format!("{site}/{page}");
    let add = |h: &Harness, kind: u8, title: &str, sites: &[&str], values: &[(&str, &str)]| {
        h.browsers
            .vault
            .with(|s| {
                s.add(Item {
                    id: String::new(),
                    revision: 0,
                    sites: sites.iter().map(|s| s.to_string()).collect(),
                    other_top_sites: Vec::new(),
                    raw: Raw::from_labels(
                        kind,
                        title,
                        &values
                            .iter()
                            .map(|(k, v)| (k.to_string(), v.to_string()))
                            .collect::<Vec<_>>(),
                    )
                    .unwrap(),
                })
            })
            .unwrap()
            .item
    };
    let login = add(
        &h,
        LOGIN,
        "Greenleaf",
        &["shop.example"],
        &[
            ("username", "testuser@example.com"),
            ("password", PASSWORD),
            ("totp", KEY),
        ],
    );
    let bare = add(
        &h,
        LOGIN,
        "Greenleaf, no password",
        &["shop.example"],
        &[("username", "other@example.com")],
    );
    let card = add(
        &h,
        CARD,
        "Test Visa",
        &[],
        &[
            ("cardholder name", "Ada Lovelace"),
            ("number", NUMBER),
            ("expiry month", "4"),
            ("expiry year", "2031"),
            ("code", "319"),
        ],
    );
    // Read through the worker, as the test's own look: the agent's eval
    // would keep the card out of this session.
    let storage = async |h: &mut Harness, key: &str| {
        h.entry
            .agent_session()
            .await
            .unwrap()
            .call(json!({"op": "eval", "script": format!("localStorage.getItem({key:?})")}))
            .await
            .unwrap()
            .result["value"]
            .clone()
    };

    // A sign-in wall in the middle of a step, the login not named: the
    // step stops needs_login, listing it, and fills nothing.
    h.open(&at("reset.html?cart=greek:Regular")).await;
    h.open(&at("cart.html")).await;
    let wall = |s: &Value| {
        if title(s).starts_with("Sign in") {
            choose("fill", Some("Greenleaf"))
        } else {
            choose("click", Some("Proceed to checkout"))
        }
    };
    told.tell(wall);
    let (code, details) = stop(
        h.send(plan(
            &["proceed to checkout -> path: checkout.html"],
            &site,
            &[],
            &[],
        ))
        .await,
    );
    println!("the wall, unnamed: {code}; {}", details["stop"]);
    assert_eq!(code, "needs_login");
    let listed: Vec<&str> = details["stop"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|i| i["item"].as_str())
        .collect();
    assert!(listed.contains(&login.as_str()), "{listed:?}");
    assert!(
        !details["stop"]["suggestions"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        told.offered(1, "fill").is_empty(),
        "no fill to choose for an unnamed item"
    );

    // Named: the wall is signed through as one fill, both stages (the
    // `username webauthn` field, then the password page), and the step goes
    // on to the checkout.
    h.open(&at("cart.html")).await;
    told.tell(wall);
    let went = h
        .send(plan(
            &["proceed to checkout -> path: checkout.html"],
            &site,
            &[],
            std::slice::from_ref(&login),
        ))
        .await
        .unwrap();
    println!(
        "the wall, named: {} trail {}",
        went["status"], went["trail"]
    );
    assert_eq!(went["status"], "reached", "{went:#}");
    let fill = went["trail"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["op"] == "fill")
        .unwrap();
    assert_eq!(fill["chosen"], "model");
    assert_eq!(fill["stages"], 2);
    assert!(fill["suggestion"].as_str().unwrap().starts_with('s'));
    let offered = told.offered(1, "fill");
    println!("offered: {offered:?}");
    assert!(
        offered
            .iter()
            .any(|o| o.starts_with("\"Greenleaf\" (login)"))
    );
    assert!(
        !offered.iter().any(|o| o.contains("no password")),
        "only named items"
    );
    assert!(offered.last().unwrap().starts_with("None of these"));
    assert!(
        storage(&mut h, "user")
            .await
            .as_str()
            .unwrap()
            .contains("testuser@example.com")
    );

    // A sign-in form for the named login with no complete suggestion (two
    // submit buttons, so none to press): needs_fields with the form's
    // fields, not needs_login.
    h.open(&at("reset.html")).await;
    h.open(&at("login-two-buttons.html")).await;
    told.tell(|_| choose("fill", None));
    let (code, details) = stop(
        h.send(plan(
            &["sign in -> path: account.html"],
            &site,
            &[],
            std::slice::from_ref(&login),
        ))
        .await,
    );
    println!(
        "named login, no complete suggestion: {code}; {}",
        details["stop"]
    );
    assert_eq!(code, "needs_fields");
    assert!(
        details["stop"]["candidates"]
            .to_string()
            .contains("Password")
    );
    assert!(details["stop"]["suggestions"][0]["press"].is_null());

    // A code stage: needs_fields before the model is asked, the code field
    // named; the agent's literal fill of `otp` then goes on.
    h.open(&at("reset.html?user=testuser@example.com")).await;
    h.open(&at("verify.html?next=account.html")).await;
    let asked = told.passes();
    let stopped = h
        .send(plan(
            &["enter the verification code"],
            &site,
            &[],
            std::slice::from_ref(&login),
        ))
        .await
        .expect_err("the plan should stop");
    let (code, details) = (stopped.code.as_str(), &stopped.details);
    println!(
        "code stage: {code}; {}; {}",
        stopped.message, details["stop"]
    );
    assert_eq!(code, "needs_fields");
    // The stop says each way on, pointing to the skill for the details:
    // the vault's code, one the site sent, typed, then the owner.
    for way in [
        "--field otp=eN",
        "`latch skill`",
        "type eN",
        "handoff --reason 2fa",
    ] {
        assert!(stopped.message.contains(way), "{way}: {}", stopped.message);
    }
    assert_eq!(told.passes(), asked, "stopped before the model was asked");
    assert_eq!(details["stop"]["page_offers"], json!(["code"]));
    assert!(
        details["stop"]["candidates"]
            .to_string()
            .contains("Verification code")
    );
    let page = h.call("observe", json!({})).await;
    let step = format!(
        "fill --item {login} --field otp={} --press {} -> path: account.html",
        super::find(&page, "textbox", "Verification code"),
        super::find(&page, "button", "Verify")
    );
    let verified = h
        .send(json!({"plan": {"steps": [short_step(&step).unwrap()]}}))
        .await
        .unwrap();
    println!("literal otp fill: {}", verified["status"]);
    assert_eq!(verified["status"], "reached", "{verified:#}");
    assert_eq!(storage(&mut h, "verified").await, "true");

    // A passkey-first page: with no fitting named item that holds a
    // password, needs_fields before the model is asked; with one, the model
    // takes the password path and the plan signs in.
    for items in [vec![], vec![bare.clone()]] {
        h.open(&at("reset.html")).await;
        h.open(&at("passkey.html?next=account.html")).await;
        let asked = told.passes();
        let stopped = h
            .send(plan(&["sign in -> path: account.html"], &site, &[], &items))
            .await
            .expect_err("the plan should stop");
        let (code, details) = (stopped.code.as_str(), &stopped.details);
        println!(
            "passkey page, items {items:?}: {code}; offers {}",
            details["stop"]["page_offers"]
        );
        assert_eq!(code, "needs_fields");
        assert!(stopped.message.contains("handoff --reason passkey"));
        assert_eq!(told.passes(), asked);
        assert_eq!(details["stop"]["page_offers"], json!(["passkey"]));
    }
    h.open(&at("reset.html")).await;
    h.open(&at("passkey.html?next=account.html")).await;
    told.tell(|s| {
        if title(s).starts_with("Sign in with a passkey") {
            choose("click", Some("Use your password instead"))
        } else {
            choose("fill", Some("Greenleaf"))
        }
    });
    let signed = h
        .send(plan(
            &["sign in -> path: account.html"],
            &site,
            &[],
            std::slice::from_ref(&login),
        ))
        .await
        .unwrap();
    println!(
        "passkey page, login named: {} trail {}",
        signed["status"], signed["trail"]
    );
    assert_eq!(signed["status"], "reached", "{signed:#}");
    assert!(!told.offered(0, "operation").contains(&"fill".to_string()));

    // A card the plan names: the model's choice of latch's suggestion fills
    // it, pressing nothing; its next choice, Pay, pays.
    h.open(&at(
        "reset.html?user=testuser@example.com&cart=greek:Regular",
    ))
    .await;
    h.open(&at("payment.html")).await;
    let passes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = passes.clone();
    told.tell(
        move |_| match seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
            0 => choose("fill", Some("Test Visa")),
            1 => choose("click", Some("Pay $")),
            _ => complete("Order GL-1042 is placed", "done"),
        },
    );
    let sent = h
        .send(plan(
            &["pay for the order with my saved card"],
            &site,
            &[],
            std::slice::from_ref(&card),
        ))
        .await;
    let paid = match sent {
        Ok(paid) => paid,
        // The risk check may stop Pay for the agent's confirmation.
        Err(stop) if stop.code == "confirm_needed" => {
            let id = stop.details["stop"]["confirmation"]["id"].clone();
            h.confirm(id.as_str().unwrap()).await.unwrap()
        }
        Err(stop) => panic!("{} {}", stop.code, stop.message),
    };
    println!("paid: {} {}", paid["status"], paid["page"]["title"]);
    // Before Pay, the model saw the card in its fields, never a value.
    assert!(
        told.offered(1, "click")
            .contains(&"textbox \"Card number\", filled from the vault".to_string()),
        "{:?}",
        told.offered(1, "click")
    );
    assert!(
        storage(&mut h, "order")
            .await
            .as_str()
            .unwrap()
            .contains("\"paid\":true")
    );

    // No value in any decision request or audit record.
    let states = told
        .states
        .lock()
        .unwrap()
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    let audits = h
        .audits
        .iter()
        .map(|a| Value::Object(a.clone()).to_string())
        .collect::<Vec<_>>()
        .join("\n");
    for value in [PASSWORD, KEY, NUMBER] {
        assert!(
            !states.contains(value) && !audits.contains(value),
            "a value reached the model or the audit"
        );
    }
    h.close().await;
}

/// A page a step's click opens in a new tab is followed: the plan goes on
/// there, in the session's own tab.
#[tokio::test]
async fn a_popup_a_step_opens_is_followed() {
    let (_dir, state, _area) = test_state("cft");
    let site = serve_shop();
    let told = Arc::new(Told::default());
    let mut h = Harness::start(&state, "cft", Some(told.clone())).await;
    h.open(&format!("{site}/recipes.html")).await;
    told.tell(|_| choose("click", Some("Open the Greek Salad recipe")));
    let went = h
        .send(plan(
            &["open the Greek Salad recipe -> text: Toss the leaves"],
            &site,
            &[],
            &[],
        ))
        .await
        .unwrap();
    println!("popup: {} trail {}", went["status"], went["trail"]);
    assert_eq!(went["status"], "reached", "{went:#}");
    let followed = went["trail"][0]["followed"].as_str().unwrap().to_string();
    let seen = h.call("observe", json!({})).await;
    println!("after: {} tabs {}", seen["title"], seen["tabs"]);
    assert!(
        seen["title"]
            .as_str()
            .unwrap()
            .starts_with("Recipe: Greek Salad")
    );
    let tab = seen["tabs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["tab"] == followed.as_str())
        .unwrap();
    assert_eq!(
        (tab["active"].as_bool(), tab["opener"].as_str()),
        (Some(true), Some("t1"))
    );
    h.close().await;
}

/// A plan stopped for the owner (a CAPTCHA) goes on in a new plan once the
/// owner has had the session through `handoff` and handed it back.
#[tokio::test]
async fn a_plan_stopped_for_the_owner_goes_on_after_the_handoff() {
    use crate::browser::handoff;
    let (_dir, state, _area) = test_state("cft");
    let site = serve_shop();
    let told = Arc::new(Told::default());
    let mut h = Harness::start(&state, "cft", Some(told.clone())).await;
    h.open(&format!(
        "{site}/reset.html?user=testuser@example.com&cart=greek:Regular&captcha=1"
    ))
    .await;
    h.open(&format!("{site}/cart.html")).await;
    told.tell(|_| choose("click", Some("Proceed to checkout")));
    let steps = ["proceed to checkout -> path: checkout.html"];
    let (code, details) = stop(h.send(plan(&steps, &site, &[], &[])).await);
    println!("captcha: {code} {}", details["stop"]);
    assert_eq!(code, "needs_owner");

    // The agent hands the session to the owner, who acts in its window and
    // hands it back on the host.
    let waiting = {
        let (browsers, id) = (h.browsers.clone(), h.id.clone());
        tokio::spawn(async move {
            let mut audit = serde_json::Map::new();
            super::host_call(
                &browsers,
                super::CALLER.into(),
                "handoff",
                &id,
                &json!({"reason": "captcha", "within": 60}),
                None,
                &mut audit,
            )
            .await
        })
    };
    let started = std::time::Instant::now();
    while handoff::pending(&state).is_empty() && started.elapsed().as_secs() < 20 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(handoff::pending(&state).len(), 1);
    handoff::done(&state, None).unwrap();
    let (handed, _) = waiting.await.unwrap().unwrap();
    println!("handed back: {}", handed["handed_back"]);
    assert_eq!(handed["handed_back"], true);

    // What the owner did (the challenge passed), stood in for here; then a
    // new plan goes on to the checkout.
    h.call(
        "eval",
        json!({"script": "localStorage.removeItem('captcha'); 1"}),
    )
    .await;
    h.open(&format!("{site}/cart.html")).await;
    let went = h.send(plan(&steps, &site, &[], &[])).await.unwrap();
    println!("after the handoff: {}", went["status"]);
    assert_eq!(went["status"], "reached", "{went:#}");
    h.close().await;
}

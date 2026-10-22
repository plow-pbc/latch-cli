//! Instruction steps: a goal in words, carried out pass by pass. Each pass
//! reads the page (`view`), asks the decision model once (`Model::decide`)
//! and then applies the stop conditions in this order:
//!
//! 1. a CAPTCHA page: `needs_owner`; a code stage, or a passkey stage
//!    unless an item the plan names fits and holds a password:
//!    `needs_fields` (latch can't tell a texted code from an
//!    authenticator code, and holds no passkeys);
//! 2. completion: a typed check that holds (`checked`, or `already_held`
//!    before the first action; once the step has acted and is about to
//!    stop, the check gets up to 10 s, and where the model judged the step
//!    complete and it does not hold, `check_not_met`), or else the model's
//!    `complete` with an
//!    evidence line (`model_assessed`; before the first action
//!    `already_held` only when that line is a state line: a field, the
//!    address or the title);
//! 3. `cannot`: `blocked`;
//! 4. `done` (offered only to a step without a typed check, once it has
//!    acted): `unconfirmed`; `stuck`: `stuck`, or `already_true` when the
//!    step's result shows only in page text before it acted;
//! 5. budgets: 8 actions a step, 30 a plan, the plan's time (and 2 waits
//!    a step, after which waiting is not offered);
//! 6. a chosen target below p 0.5: `ambiguous_target`;
//! 7. typing with none of the step's own values fitting: `needs_text`
//!    (a step's values are its quoted texts and the plan's facts it names
//!    as `{NAME}`; it types nothing else);
//! 8. a fill on a sign-in page whose fitting login the plan does not
//!    name: `needs_login`;
//! 9. a fill with no suggestion chosen (`none`, or none complete for an
//!    item the plan names): `needs_fields`, with the form's refs; a
//!    chosen one fills (`fill.rs`), or, releasing a card or identity
//!    secret, stops `confirm_needed` with its mapping;
//! 10. in a step without a typed check, a second click or submit of the
//!     same control (Enter in a field is its form's submit control, as a
//!     click on that control is): `repeat` (whether the first did the step
//!     is the model's to judge); the same action on the same page state again:
//!     `repeat`; two actions in a row that leave it unchanged:
//!     `no_progress`;
//! 11. a click or Enter that can't be undone: `confirm_needed`, through
//!     the same risk decision and confirmations as a literal step.
//!
//! An action goes through the literal steps' dispatch, journaled before
//! and after it is sent. A decision made before a fill paused the browser
//! is dropped and the page read again. Every typed string is a supplied
//! value or fact.

use std::collections::{BTreeMap, HashSet};

use serde_json::{Value, json};

use super::*;
use crate::browser::model::{Decision, Offer, Pass, Pick};
use crate::browser::plan::Step;
use crate::browser::suggest;
use crate::model::REQUEST_BUDGET;
use crate::vault::sites;

const STEP_ACTIONS: usize = 8;
const PLAN_ACTIONS: usize = 30;
const STEP_WAITS: usize = 2;
/// How long the `wait` operation waits.
const WAIT: Duration = Duration::from_millis(1500);
/// A chosen target needs at least this probability.
const TARGET_P: f64 = 0.5;
/// The most options one target question offers.
const MAX_OFFERS: usize = 250;
/// How long a typed check is polled after each action before the next
/// decision. Not the full 10 s a check gets at the step's end: a step may
/// take up to 8 actions, and most are steps on the way (typing before
/// Enter, opening a menu), whose check is not meant to hold yet.
const AFTER_ACTION: Duration = Duration::from_secs(2);
/// The most lines a pass marks as new since its step began.
const NEW_LINES: usize = 20;

/// What every instruction step of a plan sees of the plan.
pub(super) struct Context {
    /// Each step in words, in order.
    pub plan: Vec<String>,
    /// The vault items the plan names.
    pub items: Vec<String>,
}

/// One target the model may choose, and what acting on it takes.
#[derive(Clone)]
struct Target {
    offer: Offer,
    /// The element's ref, or the URL for `navigate`.
    at: String,
    /// What `type` types, or the option `select` picks.
    text: Option<String>,
    /// The element as an action line names it.
    label: String,
    role: Value,
    name: Value,
}

/// The page as one pass reads it, with what it offers.
struct Seen {
    view: Value,
    clicks: Vec<Target>,
    types: Vec<Target>,
    fields: Vec<Target>,
    selects: Vec<Target>,
    urls: Vec<Target>,
    /// Card and identity items that fit the page, by handle and title.
    items: Vec<(String, String, Vec<String>)>,
    form: Vec<Value>,
    /// What a fill could use on the page (`fill::offered`).
    fill: Value,
    /// What the page asks for.
    offers: Vec<String>,
    /// latch's complete suggestions for the items the plan names.
    bundles: Vec<Bundle>,
}

/// A fill the model may choose: one of latch's suggestions for an item the
/// plan names, with its item, mapping and press button.
struct Bundle {
    offer: Offer,
    /// The fill, as `fill.rs` takes it.
    args: Value,
    title: String,
    /// It releases a card or identity secret: confirmed first.
    confirm: bool,
}

impl Run<'_> {
    /// Carries out instruction step `n` (`at` in `context.plan`). A step
    /// with a typed check that stops once it has acted is the end of that
    /// step's actions: its check gets up to 10 s to hold, and holds, or the
    /// stop stands (`check_not_met` where the model judged it complete).
    pub(super) async fn goal(
        &mut self,
        n: usize,
        at: usize,
        step: &Step,
        context: &Context,
    ) -> Result<Done, Failure> {
        let mut acted = 0;
        let outcome = self.passes(n, at, step, context, &mut acted).await;
        let typed = step
            .check
            .as_ref()
            .filter(|c| c.kind != CheckKind::Sentence);
        let (Some(check), Err(failure)) = (typed, &outcome) else {
            return outcome;
        };
        if acted == 0 || failure.exit == Exit::Unknown {
            return outcome;
        }
        let besides = match check.kind {
            CheckKind::Download => self.downloads().await.unwrap_or_default(),
            _ => Vec::new(),
        };
        let anchor = Literal {
            snapshot: None,
            ..Literal::click("e1")
        };
        let within = CHECK_WITHIN.min(self.left());
        if self.holds(check, &anchor, besides, within).await? {
            return Ok(Done {
                outcome: "checked",
                evidence: Some(json!({"check": check.kind, "value": check.value})),
            });
        }
        outcome
    }

    async fn passes(
        &mut self,
        n: usize,
        at: usize,
        step: &Step,
        context: &Context,
        acted: &mut usize,
    ) -> Result<Done, Failure> {
        let goal = step.goal.clone().unwrap_or_default();
        let typed = step
            .check
            .as_ref()
            .filter(|c| c.kind != CheckKind::Sentence);
        let sentence = step
            .check
            .as_ref()
            .filter(|c| c.kind == CheckKind::Sentence)
            .map(|c| c.value.clone());
        // Each of the step's values with where it came from, as the model
        // reads it: quoted in the step, or a plan's fact it names.
        let values: Vec<(String, String)> = step
            .values
            .iter()
            .map(|(name, text)| {
                let quoted = name
                    .strip_prefix('v')
                    .is_some_and(|n| n.parse::<u32>().is_ok());
                let from = if quoted {
                    "quoted in this step".to_string()
                } else {
                    format!("the plan's fact {name:?}")
                };
                (from, text.clone())
            })
            .collect();
        let anchor = Literal {
            snapshot: None,
            ..Literal::click("e1")
        };
        let besides = match typed {
            Some(c) if c.kind == CheckKind::Download => self.downloads().await?,
            _ => Vec::new(),
        };
        if let Some(check) = typed
            && check.require_change
            && self
                .holds(check, &anchor, besides.clone(), Duration::ZERO)
                .await?
        {
            return Err(stopped(
                "check_not_met",
                format!(
                    "step {n}'s check already holds before the step, and it must change; nothing was sent"
                ),
            ));
        }
        let mut waits = 0;
        let mut lines: Vec<String> = Vec::new();
        // The page state before the latest action, and how many actions in
        // a row left it unchanged.
        let mut before: Option<String> = None;
        let mut unchanged = 0;
        // The field typed into last, for Enter.
        let mut typed_into: Option<Typed> = None;
        // The page's lines when the step began, and the mark the worker
        // counts what is written since by.
        let mut began: Option<HashSet<String>> = None;
        let mark = format!("{}-{n}", self.request);
        // The controls this step has clicked or submitted: without a typed
        // check, each at most once.
        let mut tried: HashSet<String> = HashSet::new();
        loop {
            if self.left().is_zero() {
                return Err(stopped(
                    "deadline",
                    format!("the plan's time ran out during step {n}"),
                ));
            }
            let epoch = self.entry.runtime.pauses.load(Ordering::SeqCst);
            let seen = self
                .see((&mark, began.is_none()), step, &values, context)
                .await?;
            let view = &seen.view;
            let state = view["state"].as_str().unwrap_or_default().to_string();
            // New since the step began: a line written on screen since (its
            // text set again counts, even unchanged), or text that was not
            // on the page then.
            let on_screen = strings(&view["on_screen"]);
            let fresh: Vec<bool> = view["fresh"]
                .as_array()
                .map(|f| f.iter().map(|b| b == true).collect())
                .unwrap_or_default();
            let shown: Vec<String> = on_screen
                .iter()
                .cloned()
                .chain(strings(&view["lines"]))
                .collect();
            let began = began.get_or_insert_with(|| shown.iter().cloned().collect());
            let new_lines: Vec<String> = shown
                .iter()
                .enumerate()
                .filter(|(k, line)| fresh.get(*k) == Some(&true) || !began.contains(*line))
                .map(|(_, line)| line.clone())
                .take(NEW_LINES)
                .collect();
            if let Some(was) = before.take() {
                unchanged = if was == state { unchanged + 1 } else { 0 };
            }

            // 1. A CAPTCHA.
            if view["wall"] == "captcha" {
                let mut failure = stopped(
                    "needs_owner",
                    format!("step {n} reached a CAPTCHA; hand off to the owner"),
                );
                failure.details = json!({"reason": "captcha"});
                return Err(failure);
            }
            // 1. A code or passkey stage: the agent chooses between a fill
            // of `otp` and a handoff. A passkey-first page goes on when a
            // named item can take its password path.
            let offers: Vec<&str> = seen.offers.iter().map(String::as_str).collect();
            let password_path = seen.fill["items"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|item| {
                    item["kind"] == "login"
                        && context
                            .items
                            .iter()
                            .any(|named| item["item"] == named.as_str())
                        && item["fields"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .any(|f| f["id"] == "password")
                });
            if suggest::code_stage(&offers) || (suggest::passkey_stage(&offers) && !password_path) {
                let recovery = if suggest::code_stage(&offers) {
                    "a one-time code; nothing was sent. If it is the saved login's authenticator code, send `fill --item vN --field otp=eN --press eN`. If the site texted it to the owner and your latch runs on macOS with a host that reads Messages, find it with `latch messages search --since 10m code` and enter it as a literal step, `do 'type eN \"CODE\"'`, then submit as the page asks. On a Linux latch, or if neither works, hand off to the owner (`handoff --reason 2fa`)"
                } else {
                    "a passkey; nothing was sent. Hand off to the owner (`handoff --reason passkey`)"
                };
                let mut failure = stopped(
                    "needs_fields",
                    format!("step {n} reached a page asking for {recovery}"),
                );
                failure.details = fields_stop(&seen);
                return Err(failure);
            }

            // 2. A typed check decides completion. One that must change is
            // never held before the step acts.
            if let Some(check) = typed
                && (*acted > 0 || !check.require_change)
                && self
                    .holds(check, &anchor, besides.clone(), Duration::ZERO)
                    .await?
            {
                return Ok(Done {
                    outcome: if *acted == 0 {
                        "already_held"
                    } else {
                        "checked"
                    },
                    evidence: Some(json!({"check": check.kind, "value": check.value})),
                });
            }

            let pass = self.pass(
                typed,
                new_lines,
                waits,
                at,
                &goal,
                &sentence,
                &values,
                &seen,
                &lines,
                *acted,
                &typed_into,
                context,
            );
            if !self.model.is_on() {
                return Err(model_unavailable(&self.model, "an instruction step"));
            }
            if let Some(size) = model::too_large(&pass) {
                let mut failure = stopped(
                    "request_too_large",
                    format!(
                        "step {n}'s decision request comes to {size} bytes even with every element and page line left out, more than the {} the decision route takes; nothing was sent. Shorten the plan or the step's texts",
                        REQUEST_BUDGET
                    ),
                );
                failure.details = json!({"size": size, "budget": REQUEST_BUDGET});
                return Err(failure);
            }
            let decided = self.model.decide(&pass).await;
            record_model(&self.model, self.audit);
            let decision =
                decided.map_err(|_| model_unavailable(&self.model, "an instruction step"))?;
            let model_evidence = |decision: &Decision| {
                let mut evidence = json!({
                    "check": if sentence.is_some() { "sentence" } else { "model" },
                    "p": model::round(decision.status_p),
                });
                if let Some((line, _)) = &decision.evidence {
                    evidence["evidence"] = json!(line);
                }
                evidence
            };

            // 2. The model's completion. With a typed check it is the check's
            // to decide: once the step has acted, it gets its wait (`goal`).
            if let Some(check) = typed
                && *acted > 0
                && decision.complete()
            {
                let mut failure = stopped(
                    "check_not_met",
                    format!(
                        "step {n} was judged complete, and its check did not hold within {}s",
                        CHECK_WITHIN.as_secs()
                    ),
                );
                failure.details = json!({"evidence": {"check": check.kind, "value": check.value,
                                                      "model": model_evidence(&decision)}});
                return Err(failure);
            }
            if typed.is_none() && decision.complete() {
                let state_line = decision
                    .evidence
                    .as_ref()
                    .is_some_and(|(_, kind)| kind.is_state());
                if *acted > 0 {
                    return Ok(Done {
                        outcome: "model_assessed",
                        evidence: Some(model_evidence(&decision)),
                    });
                }
                if state_line {
                    return Ok(Done {
                        outcome: "already_held",
                        evidence: Some(model_evidence(&decision)),
                    });
                }
            }

            // 3. It can't be done here.
            if decision.status == "cannot" {
                let mut failure = stopped(
                    "blocked",
                    format!(
                        "step {n} can't be done on this page; the rest of the plan did not run"
                    ),
                );
                failure.details = json!({"evidence": {"p": model::round(decision.status_p),
                                                      "lines": decision.top_lines}});
                return Err(failure);
            }

            // 4. Done, or stuck.
            match decision.operation.id.as_str() {
                "done" => {
                    let mut failure = stopped(
                        "unconfirmed",
                        format!(
                            "step {n} acted and the model judged it finished, but nothing on the page confirms it; look at the page"
                        ),
                    );
                    failure.details = json!({"evidence": {"p": model::round(decision.status_p),
                                                          "lines": decision.top_lines}});
                    return Err(failure);
                }
                "stuck" => {
                    if *acted == 0 && typed.is_none() && decision.complete() {
                        let mut failure = stopped(
                            "already_true",
                            format!(
                                "step {n}'s result seems to show already, but only in page text, which an earlier step may have left; nothing was sent"
                            ),
                        );
                        failure.details = json!({"evidence": model_evidence(&decision)});
                        return Err(failure);
                    }
                    let mut failure = stopped(
                        "stuck",
                        format!("no operation this page offers makes progress on step {n}"),
                    );
                    failure.details = json!({"evidence": {"lines": decision.top_lines}});
                    return Err(failure);
                }
                _ => {}
            }

            // 5. Budgets.
            let op = decision.operation.id.clone();
            if op == "wait" {
                waits += 1;
                if waits > STEP_WAITS {
                    return Err(stopped(
                        "step_budget",
                        format!("step {n} waited {STEP_WAITS} times already"),
                    ));
                }
                tokio::time::sleep(WAIT.min(self.left())).await;
                lines.push("waited".into());
                continue;
            }
            if *acted >= STEP_ACTIONS {
                return Err(stopped(
                    "step_budget",
                    format!("step {n} took {STEP_ACTIONS} actions without finishing"),
                ));
            }
            if self.actions >= PLAN_ACTIONS {
                return Err(stopped(
                    "plan_budget",
                    format!("the plan took {PLAN_ACTIONS} actions"),
                ));
            }

            // 6. The chosen target.
            let (pick, offered) = match op.as_str() {
                "click" => (&decision.click, &seen.clicks),
                "type" => (&decision.typed, &seen.types),
                "select" => (&decision.select, &seen.selects),
                "navigate" => (&decision.url, &seen.urls),
                _ => (&None, &seen.clicks),
            };
            let chosen = match (op.as_str(), pick) {
                ("click" | "type" | "select" | "navigate", Some(pick)) => {
                    if pick.p < TARGET_P {
                        let mut failure = stopped(
                            "ambiguous_target",
                            format!(
                                "step {n}: no one target stands out for {op}; name it with a literal step on one of these refs"
                            ),
                        );
                        failure.details = json!({"candidates": candidates(pick, offered)});
                        return Err(failure);
                    }
                    offered.iter().find(|t| t.offer.id == pick.id)
                }
                _ => None,
            };

            // 7. Typing needs a supplied text that fits.
            if op == "type" && chosen.is_none() {
                let field = decision
                    .typed
                    .iter()
                    .flat_map(|p| &p.likeliest)
                    .find_map(|(id, _)| seen.types.iter().find(|t| &t.offer.id == id))
                    .or_else(|| {
                        let id = &decision.type_field.as_ref()?.id;
                        seen.fields.iter().find(|t| &t.offer.id == id)
                    })
                    .map(|t| json!({"ref": t.at, "role": t.role, "name": t.name}))
                    .or_else(|| first_field(view));
                let mut failure = stopped(
                    "needs_text",
                    format!(
                        "step {n} needs text that was not supplied; nothing was typed. Supply it in quotes in the step, or name a fact of the plan as {{NAME}}"
                    ),
                );
                failure.details = json!({"field": field});
                return Err(failure);
            }

            // 8. and 9. A fill: one of latch's suggestions, or a stop.
            if op == "fill" {
                // needs_login only when the plan names no fitting login; one
                // it names with no complete suggestion is a needs_fields.
                let sign_in = suggest::sign_in(&offers);
                let named_login = seen.fill["items"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|item| {
                        item["kind"] == "login"
                            && context
                                .items
                                .iter()
                                .any(|named| item["item"] == named.as_str())
                    });
                if sign_in && !named_login {
                    let mut failure = stopped(
                        "needs_login",
                        format!(
                            "step {n} reached a sign-in form; the rest of the plan did not run. Name the saved login in a new plan (`--item vN`), or send `fill --item vN --suggested S` or your own mapping"
                        ),
                    );
                    failure.details = json!({
                        "items": seen.fill["items"],
                        "suggestions": seen.fill["suggestions"],
                        "page_offers": seen.fill["page_offers"],
                    });
                    return Err(failure);
                }
                let chosen = decision
                    .bundle
                    .as_ref()
                    .filter(|pick| pick.id != "none")
                    .and_then(|pick| {
                        seen.bundles
                            .iter()
                            .find(|b| b.offer.id == pick.id)
                            .map(|b| (pick, b))
                    });
                let Some((pick, bundle)) = chosen else {
                    let mut failure = stopped(
                        "needs_fields",
                        format!(
                            "step {n} reached a form a saved item fits, with no suggestion of latch's to fill it; latch does not map its fields itself: send a step `fill --item vN --field FIELD=eN… --press eN` with these refs, or name the item in a new plan"
                        ),
                    );
                    failure.details = fields_stop(&seen);
                    return Err(failure);
                };
                if pick.p < TARGET_P {
                    let mut failure = stopped(
                        "ambiguous_target",
                        format!(
                            "step {n}: no one of latch's suggested fills stands out; send `fill --item vN --suggested S` for the one the step means"
                        ),
                    );
                    failure.details = json!({"suggestions": seen.fill["suggestions"]});
                    return Err(failure);
                }
                let key = format!("fill|{}", bundle.offer.id);
                if !self.dispatched.insert((key, state.clone())) {
                    let mut failure = stopped(
                        "repeat",
                        format!(
                            "step {n} would fill from {:?} again on a page that has not changed; nothing was sent",
                            bundle.title
                        ),
                    );
                    failure.details = json!({"suggestion": bundle.offer.id});
                    return Err(failure);
                }
                if self.entry.runtime.filling.load(Ordering::SeqCst)
                    || self.entry.runtime.pauses.load(Ordering::SeqCst) != epoch
                {
                    continue;
                }
                let mut args = bundle.args.clone();
                if let Some(object) = args.as_object_mut() {
                    object.remove("kind");
                }
                let judged = self.judge_fill(n, &args).await?;
                if bundle.confirm {
                    args["item_title"] = json!(bundle.title);
                    let failure = confirm_fill(self.browsers, self.entry, &args, self.audit).await;
                    let mut record = choice_record(&decision, Some(pick));
                    record["step"] = json!(n);
                    record["op"] = json!("fill");
                    record["item"] = args["item"].clone();
                    record["suggestion"] = json!(bundle.offer.id);
                    record["confirmation"] = failure.details["confirmation"]["id"].clone();
                    record["dispatch"] = json!("refused");
                    if let Some(judgment) = self.judgment.take() {
                        record["gate"] = judgment;
                    }
                    self.records.push(record);
                    let failure = Err(failure);
                    if let Some(judged) = judged {
                        judged.ended(&failure);
                    }
                    return failure;
                }
                self.choice = Some(choice_record(&decision, Some(pick)));
                let sent = self.fill_now(n, args, None).await;
                if let Some(judged) = judged {
                    judged.ended(&sent);
                }
                self.choice = None;
                *acted += 1;
                before = Some(state);
                match sent {
                    Ok(()) => lines.push(format!(
                        "filled from {:?} and pressed its button",
                        bundle.title
                    )),
                    Err(failure) if retried(&failure) => {
                        lines.push(format!(
                            "filled from {:?} (not sent: {})",
                            bundle.title, failure.code
                        ));
                    }
                    Err(failure) => return Err(failure),
                }
                if let Some(check) = typed
                    && self
                        .holds(
                            check,
                            &anchor,
                            besides.clone(),
                            AFTER_ACTION.min(self.left()),
                        )
                        .await?
                {
                    return Ok(Done {
                        outcome: "checked",
                        evidence: Some(json!({"check": check.kind, "value": check.value})),
                    });
                }
                continue;
            }

            // The action, as a literal step on this view's snapshot.
            let snapshot = view["snapshot"].as_str().map(str::to_string);
            let (literal, line, key) = match (op.as_str(), chosen) {
                ("click", Some(t)) => (
                    Literal {
                        snapshot: snapshot.clone(),
                        ..Literal::click(&t.at)
                    },
                    format!("clicked {}", t.label),
                    identity(view, &t.at),
                ),
                ("type", Some(t)) => {
                    let text = t.text.clone().unwrap_or_default();
                    (
                        Literal {
                            op: Op::Type,
                            text: Some(text.clone()),
                            snapshot: snapshot.clone(),
                            ..Literal::click(&t.at)
                        },
                        format!("typed {text:?} into {}", t.label),
                        format!("type|{}|{text}", identity(view, &t.at)),
                    )
                }
                ("select", Some(t)) => {
                    let option = t.text.clone().unwrap_or_default();
                    (
                        Literal {
                            op: Op::Select,
                            text: Some(option.clone()),
                            snapshot: snapshot.clone(),
                            ..Literal::click(&t.at)
                        },
                        format!("chose {option:?} in {}", t.label),
                        format!("select|{}|{option}", identity(view, &t.at)),
                    )
                }
                ("press_enter", _) => {
                    let Some(Typed {
                        snapshot: field_snapshot,
                        field,
                        label,
                        form,
                    }) = typed_into.clone()
                    else {
                        return Err(stopped(
                            "stuck",
                            format!("step {n}: there is no field to press Enter in"),
                        ));
                    };
                    // Enter is the control it submits: the same key as a
                    // click on it.
                    let control = self.submits(view, &field_snapshot, &field, &form).await;
                    (
                        Literal {
                            op: Op::Press,
                            text: Some("Enter".into()),
                            snapshot: Some(field_snapshot),
                            ..Literal::click(&field)
                        },
                        format!("pressed Enter in {label}"),
                        control,
                    )
                }
                ("scroll_down" | "scroll_up", _) => {
                    let way = if op == "scroll_down" { "down" } else { "up" };
                    (
                        Literal {
                            op: Op::Scroll,
                            target: None,
                            text: Some(way.into()),
                            ..Literal::click("e1")
                        },
                        format!("scrolled {way}"),
                        format!("scroll|{way}"),
                    )
                }
                ("back", _) => (
                    Literal {
                        op: Op::Back,
                        target: None,
                        ..Literal::click("e1")
                    },
                    "went back".to_string(),
                    "back".to_string(),
                ),
                ("navigate", Some(t)) => (
                    Literal::click("e1"),
                    format!("went to {}", t.at),
                    format!("navigate|{}", t.at),
                ),
                _ => {
                    return Err(stopped(
                        "stuck",
                        format!("step {n}: the model chose {op} with nothing to do it on"),
                    ));
                }
            };

            // A fill paused the browser while the model answered: read the
            // page again.
            if self.entry.runtime.filling.load(Ordering::SeqCst)
                || self.entry.runtime.pauses.load(Ordering::SeqCst) != epoch
            {
                continue;
            }

            // 10. Without a typed check, a control is clicked or submitted
            // once: whether that did the step is the model's to say, not a
            // second try's.
            let control = matches!(op.as_str(), "click" | "press_enter").then(|| key.clone());
            if typed.is_none()
                && let Some(control) = &control
                && tried.contains(control)
            {
                let mut failure = stopped(
                    "repeat",
                    format!(
                        "step {n} would {line} a second time; nothing was sent. If the step needs it, say so in a new step, or give it a typed check"
                    ),
                );
                failure.details = json!({"action": line});
                return Err(failure);
            }

            // 10. The same action on the same page again, or no change.
            if !self.dispatched.insert((key, state.clone())) {
                let mut failure = stopped(
                    "repeat",
                    format!(
                        "step {n} would {line} again on a page that has not changed; nothing was sent"
                    ),
                );
                failure.details = json!({"action": line});
                return Err(failure);
            }
            if unchanged >= 2 {
                return Err(stopped(
                    "no_progress",
                    format!(
                        "step {n}'s last two actions left the page unchanged; the rest of the plan did not run"
                    ),
                ));
            }

            // 11. The action, judged for risk where it clicks or presses Enter.
            self.choice = Some(choice_record(&decision, pick.as_ref()));
            let sent = match op.as_str() {
                "click" => self.risky(n, &literal, false).await,
                "press_enter" => self.risky(n, &literal, true).await,
                "navigate" => {
                    let url = chosen.map(|t| t.at.clone()).unwrap_or_default();
                    self.navigate(n, &url).await
                }
                _ => self.dispatch(n, &literal, None, None, None).await,
            };
            self.choice = None;
            *acted += 1;
            before = Some(state);
            let done = sent.is_ok();
            match sent {
                Ok(()) => {
                    lines.push(line);
                    tried.extend(control);
                }
                Err(failure) if retried(&failure) => {
                    lines.push(format!("{line} (not sent: {})", failure.code));
                }
                Err(failure) => return Err(failure),
            }
            if op == "type"
                && let Some(t) = chosen
            {
                typed_into = Some(Typed {
                    snapshot: snapshot.unwrap_or_default(),
                    field: t.at.clone(),
                    label: t.label.clone(),
                    form: form_key(view, &t.at),
                });
            }
            // A typed check is polled after each action, before the page is
            // read again for the next decision.
            if let Some(check) = typed
                && done
                && self
                    .holds(
                        check,
                        &anchor,
                        besides.clone(),
                        AFTER_ACTION.min(self.left()),
                    )
                    .await?
            {
                return Ok(Done {
                    outcome: "checked",
                    evidence: Some(json!({"check": check.kind, "value": check.value})),
                });
            }
        }
    }

    /// The page, and what this step may do on it.
    async fn see(
        &mut self,
        mark: (&str, bool),
        step: &Step,
        values: &[(String, String)],
        context: &Context,
    ) -> Result<Seen, Failure> {
        let (view, _) = call(
            self.entry,
            json!({"op": "view", "step": mark.0, "begin": mark.1}),
            self.audit,
        )
        .await?;
        let elements: Vec<&Value> = view["elements"].as_array().into_iter().flatten().collect();
        let usable = |e: &&&Value| {
            e["disabled"] != true
                && e["covered"] != true
                && e["hidden"] != true
                && e["gone"] != true
        };
        let mut clicks = Vec::new();
        let mut types = Vec::new();
        let mut fields = Vec::new();
        let mut selects = Vec::new();
        for e in elements.iter().filter(usable) {
            let at = e["ref"].as_str().unwrap_or_default().to_string();
            let label = describe(e);
            match e["kind"].as_str() {
                Some("select") => {
                    let now = e["value"].as_str().unwrap_or_default();
                    for (k, option) in strings(&e["options"]).into_iter().enumerate() {
                        let chosen = if option == now {
                            " (already chosen)"
                        } else {
                            ""
                        };
                        selects.push(Target {
                            offer: Offer {
                                id: format!("{at}#{k}"),
                                text: format!("{label}  →  {option:?}{chosen}"),
                            },
                            at: at.clone(),
                            text: Some(option),
                            label: label.clone(),
                            role: e["role"].clone(),
                            name: e["name"].clone(),
                        });
                    }
                }
                Some("file") => {}
                Some("text") if e["secret"] != true && e["filled"] != true => {
                    for (k, (name, text)) in values.iter().enumerate() {
                        types.push(Target {
                            offer: Offer {
                                id: format!("{at}#{k}"),
                                text: format!("{label}  ←  {text:?} ({name})"),
                            },
                            at: at.clone(),
                            text: Some(text.clone()),
                            label: label.clone(),
                            role: e["role"].clone(),
                            name: e["name"].clone(),
                        });
                    }
                    let field = Target {
                        offer: Offer {
                            id: at.clone(),
                            text: label.clone(),
                        },
                        at: at.clone(),
                        text: None,
                        label: label.clone(),
                        role: e["role"].clone(),
                        name: e["name"].clone(),
                    };
                    fields.push(field.clone());
                    clicks.push(field);
                }
                _ => clicks.push(Target {
                    offer: Offer {
                        id: at.clone(),
                        text: label.clone(),
                    },
                    at: at.clone(),
                    text: None,
                    label: label.clone(),
                    role: e["role"].clone(),
                    name: e["name"].clone(),
                }),
            }
        }
        clicks.truncate(MAX_OFFERS);
        types.truncate(MAX_OFFERS);
        fields.truncate(MAX_OFFERS);
        selects.truncate(MAX_OFFERS);
        let urls = step
            .urls
            .iter()
            .map(|(name, url)| Target {
                offer: Offer {
                    id: name.clone(),
                    text: url.clone(),
                },
                at: url.clone(),
                text: None,
                label: url.clone(),
                role: Value::Null,
                name: Value::Null,
            })
            .collect();
        let (origin, _) =
            crate::browser::core::origin_and_path(view["url"].as_str().unwrap_or_default());
        let top = self.browsers.origin_as(&origin);
        let items = self
            .browsers
            .vault
            .list_if_present()
            .into_iter()
            .filter(|meta| matches!(meta.kind, "card" | "identity"))
            .filter(|meta| {
                context.items.contains(&meta.item) || sites::allows(meta, &top, &top).is_ok()
            })
            .map(|meta| {
                let fields = meta
                    .fields
                    .iter()
                    .filter(|f| !f.alias)
                    .map(|f| f.label.clone())
                    .collect();
                (meta.item, meta.title, fields)
            })
            .collect();
        let form = form_of(&elements);
        let inventory = json!({"snapshot": view["snapshot"], "top_origin": origin,
                               "controls": view["controls"]});
        let fill = super::super::fill::offered(self.browsers, &inventory);
        let offers = strings(&fill["page_offers"]);
        let bundles = bundles(self.browsers, &fill, &elements, &context.items);
        Ok(Seen {
            view,
            clicks,
            types,
            fields,
            selects,
            urls,
            items,
            form,
            fill,
            offers,
            bundles,
        })
    }

    /// What one pass tells the model, and what it offers.
    #[allow(clippy::too_many_arguments)]
    fn pass(
        &self,
        typed: Option<&Check>,
        new_lines: Vec<String>,
        waits: usize,
        at: usize,
        goal: &str,
        sentence: &Option<String>,
        values: &[(String, String)],
        seen: &Seen,
        lines: &[String],
        acted: usize,
        typed_into: &Option<Typed>,
        context: &Context,
    ) -> Pass {
        let view = &seen.view;
        let y = view["scroll"]["y"].as_f64().unwrap_or(0.0);
        let max = view["scroll"]["max"].as_f64().unwrap_or(0.0);
        let mut operations = Vec::new();
        if !seen.clicks.is_empty() {
            operations.push("click");
        }
        if view["elements"].as_array().into_iter().flatten().any(|e| {
            e["kind"] == "text"
                && e["secret"] != true
                && e["filled"] != true
                && e["disabled"] != true
                && e["covered"] != true
                && e["hidden"] != true
        }) {
            operations.push("type");
        }
        if !seen.selects.is_empty() {
            operations.push("select");
        }
        if typed_into.is_some() && lines.last().is_some_and(|l| l.starts_with("typed ")) {
            operations.push("press_enter");
        }
        if !seen.urls.is_empty() {
            operations.push("navigate");
        }
        if y < max - 5.0 {
            operations.push("scroll_down");
        }
        if y > 5.0 {
            operations.push("scroll_up");
        }
        operations.push("back");
        // Once a step has waited its fill, waiting is no longer possible.
        if waits < STEP_WAITS {
            operations.push("wait");
        }
        // A fill, where a fitting item could take the page's sign-in fields
        // or its form; never on a passkey stage.
        let offers: Vec<&str> = seen.offers.iter().map(String::as_str).collect();
        let fitting_login = seen.fill["items"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|item| item["kind"] == "login");
        if !suggest::passkey_stage(&offers)
            && ((suggest::sign_in(&offers) && fitting_login)
                || (!seen.items.is_empty() && !seen.form.is_empty()))
        {
            operations.push("fill");
        }
        // With a typed check, latch decides when the step is done.
        if acted > 0 && typed.is_none() {
            operations.push("done");
        }
        operations.push("stuck");
        let mut notes = Vec::new();
        let fill_offered = operations.contains(&"fill");
        for item in seen.fill["items"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|_| fill_offered)
        {
            let fields: Vec<&str> = item["fields"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|f| f["id"].as_str())
                .collect();
            notes.push(format!(
                "a saved {} fits this page ({:?}, holding {}); choose fill to use it",
                item["kind"].as_str().unwrap_or("item"),
                item["title"].as_str().unwrap_or_default(),
                fields.join(", ")
            ));
        }
        if !offers.is_empty() {
            notes.push(format!("the page asks for: {}", offers.join(", ")));
        }
        // A passkey stage gets this far only for a named login with a
        // password: latch has no passkey to give, the password path is the
        // way in.
        if suggest::passkey_stage(&offers) {
            notes.push(
                "the saved login signs in with its password, not a passkey: go to the page's password sign-in, where it can be filled"
                    .into(),
            );
        }
        if view["truncated"] == true {
            notes.push("the element list was cut short".into());
        }
        let scroll = if max <= 0.0 || y < 5.0 {
            "top"
        } else if y >= max - 5.0 {
            "bottom"
        } else {
            "middle"
        };
        Pass {
            plan: context
                .plan
                .iter()
                .enumerate()
                .map(|(k, text)| {
                    let status = match k.cmp(&at) {
                        std::cmp::Ordering::Less => "done",
                        std::cmp::Ordering::Equal => "current",
                        std::cmp::Ordering::Greater => "later",
                    };
                    (text.clone(), status)
                })
                .collect(),
            step: goal.to_string(),
            check: sentence.clone(),
            outcome: typed.map(described),
            values: values.to_vec(),
            url: view["url"].as_str().unwrap_or_default().to_string(),
            title: view["title"].as_str().unwrap_or_default().to_string(),
            on_screen: strings(&view["on_screen"]),
            lines: strings(&view["lines"]),
            new_lines,
            fields: strings(&view["fields"]),
            scroll,
            actions: lines.iter().rev().take(8).rev().cloned().collect(),
            notes,
            operations,
            clicks: seen.clicks.iter().map(|t| t.offer.clone()).collect(),
            types: seen.types.iter().map(|t| t.offer.clone()).collect(),
            type_fields: seen.fields.iter().map(|t| t.offer.clone()).collect(),
            selects: seen.selects.iter().map(|t| t.offer.clone()).collect(),
            urls: seen.urls.iter().map(|t| t.offer.clone()).collect(),
            bundles: seen.bundles.iter().map(|b| b.offer.clone()).collect(),
        }
    }

    /// What Enter in `field` submits, keyed as the control a click would
    /// act on: its form's submit control as the page lists it, else its
    /// form, by the field.
    async fn submits(&mut self, view: &Value, snapshot: &str, field: &str, form: &str) -> String {
        let reviewed = call(
            self.entry,
            json!({"op": "review", "snapshot": snapshot, "target": field, "enter": true}),
            self.audit,
        )
        .await
        .map(|(reviewed, _)| reviewed)
        .unwrap_or_default();
        let acts = &reviewed["acts_on"];
        if acts["role"] != "form" && acts["name"].as_str().is_some_and(|n| !n.is_empty()) {
            // The submit control of the field's own form, of those with its
            // role and name.
            let listed = view["elements"].as_array().into_iter().flatten().find(|e| {
                e["role"] == acts["role"]
                    && e["name"] == acts["name"]
                    && form_key(view, e["ref"].as_str().unwrap_or_default()) == form
            });
            return match listed {
                Some(e) => identity(view, e["ref"].as_str().unwrap_or_default()),
                None => format!("{}|{}|{form}", acts["role"], acts["name"]),
            };
        }
        format!("form|{form}")
    }

    /// Goes to one of the step's URLs, journaled as an action.
    async fn navigate(&mut self, n: usize, url: &str) -> Result<(), Failure> {
        let action = self.begin_named(n, json!("navigate"), json!({"url": url}))?;
        let reply = call(self.entry, json!({"op": "open", "url": url}), self.audit).await;
        let result = match &reply {
            Ok(_) => "dispatched",
            Err(failure) if refused(failure) => "refused",
            Err(_) => "unknown",
        };
        self.end(action, result, &Value::Null)?;
        let mut record = json!({"step": n, "action": action, "op": "navigate",
                                "dispatch": if result == "dispatched" { "sent" } else { result }});
        let mut shown =
            json!({"step": n, "action": action, "op": "navigate", "url": url, "result": result});
        if let Some(choice) = &self.choice {
            for (key, value) in choice.as_object().into_iter().flatten() {
                record[key] = value.clone();
                shown[key] = value.clone();
            }
        }
        if result == "dispatched" {
            self.shoot(action, &mut record, &mut shown).await;
        }
        self.records.push(record);
        self.trail.push(shown);
        reply.map(|_| ())
    }
}

/// A typed check in words, as the model reads what the page will show.
fn described(check: &Check) -> String {
    let value = &check.value;
    let shown = match check.kind {
        CheckKind::Origin => format!("the page is on {value}"),
        CheckKind::Path => format!("the page's address path contains {value:?}"),
        CheckKind::Query => format!("the page's address has the query {value:?}"),
        CheckKind::Text => format!("the page shows the text {value:?}"),
        CheckKind::Field => format!("a field holds {value:?}"),
        CheckKind::Download => "a download has completed".to_string(),
        CheckKind::Sentence => value.clone(),
    };
    if check.require_change {
        format!("{shown}, which it did not before the step")
    } else {
        shown
    }
}

/// How the model chose an action, for its audit record and trail entry.
fn choice_record(decision: &Decision, pick: Option<&Pick>) -> Value {
    let mut choice = json!({"chosen": "model", "operation_p": model::round(decision.operation.p)});
    if let Some((elements, lines)) = decision.trimmed {
        choice["left_out"] = json!({"elements": elements, "lines": lines});
    }
    if let Some(pick) = pick {
        choice["target_p"] = json!(model::round(pick.p));
    }
    choice
}

/// A refusal made before any input on a target the model chose from a
/// page that has since moved on: the step goes on, and the action counts.
fn retried(failure: &Failure) -> bool {
    matches!(
        failure.code.as_str(),
        "stale_snapshot" | "covered_target" | "no_such_target"
    )
}

/// An element as the model and the action lines read it.
fn describe(e: &Value) -> String {
    let name = e["name"]
        .as_str()
        .filter(|n| !n.is_empty())
        .unwrap_or("(unlabelled)");
    let mut line = format!("{} {name:?}", e["role"].as_str().unwrap_or("element"));
    if e["kind"] == "select" {
        line.push_str(&format!(
            ", showing {:?}",
            e["value"].as_str().unwrap_or_default()
        ));
    } else if let Some(value) = e["value"].as_str() {
        line.push_str(&format!(", value {value:?}"));
    } else if e["kind"] == "text" {
        line.push_str(", empty");
    }
    match e["checked"].as_bool() {
        Some(true) => line.push_str(", checked"),
        Some(false) => line.push_str(", not checked"),
        None => {}
    }
    if e["offscreen"] == true {
        line.push_str(", off-screen");
    }
    if let Some(row) = e["row"].as_str() {
        line.push_str(&format!(" — in: {row}"));
    }
    line
}

/// What makes an action on `target` the same action on another reading
/// of the page: the element's role, name, row and form, not its ref or
/// value.
fn identity(view: &Value, target: &str) -> String {
    let e = element(view, target);
    format!(
        "{}|{}|{}|{}",
        e["role"],
        e["name"],
        e["row"],
        form_key(view, target)
    )
}

/// The form `target` is in, by frame and the form's place in its
/// document; `null|null` outside any form.
fn form_key(view: &Value, target: &str) -> String {
    let e = element(view, target);
    format!("{}|{}", e["frame"], e["form"])
}

fn element(view: &Value, target: &str) -> Value {
    view["elements"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|e| e["ref"] == target)
        .cloned()
        .unwrap_or_default()
}

/// The field typed into last, for Enter: its ref in the snapshot it was
/// typed in, how an action line names it, and its form.
#[derive(Clone)]
struct Typed {
    snapshot: String,
    field: String,
    label: String,
    form: String,
}

/// A target question's likeliest options, as refs the agent can name.
fn candidates(pick: &Pick, offered: &[Target]) -> Vec<Value> {
    pick.likeliest
        .iter()
        .filter_map(|(id, p)| {
            offered.iter().find(|t| &t.offer.id == id).map(|t| {
                let mut candidate =
                    json!({"ref": t.at, "role": t.role, "name": t.name, "p": model::round(*p)});
                if let Some(text) = &t.text {
                    candidate["text"] = json!(text);
                }
                candidate
            })
        })
        .collect()
}

/// latch's suggestions for the items the plan names, as fills the model
/// may choose: complete ones only (a press button, which every login and
/// every secret needs), each read as the item, its mapping by the page's
/// names, and its button.
fn bundles(
    browsers: &Browsers,
    fill: &Value,
    elements: &[&Value],
    named: &[String],
) -> Vec<Bundle> {
    let name_of = |reference: &str| {
        elements
            .iter()
            .find(|e| e["ref"] == reference)
            .map(|e| describe(e))
            .unwrap_or_else(|| reference.to_string())
    };
    let metas = browsers.vault.list_if_present();
    let mut made = Vec::new();
    for shown in fill["suggestions"].as_array().into_iter().flatten() {
        let Some(meta) = metas
            .iter()
            .find(|m| shown["item"] == m.item.as_str() && named.contains(&m.item))
        else {
            continue;
        };
        let Some(press) = shown["press"].as_str() else {
            continue;
        };
        let fields: Vec<(String, Vec<String>)> = shown["fields"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(label, refs)| (label.clone(), strings(refs)))
            .collect();
        let secret = fields
            .iter()
            .any(|(label, _)| suggest::secret_field(meta, label));
        let mapped: Vec<String> = fields
            .iter()
            .map(|(label, refs)| {
                let names: Vec<String> = refs.iter().map(|r| name_of(r)).collect();
                format!("{label} → {}", names.join(" + "))
            })
            .collect();
        let id = shown["id"].as_str().unwrap_or_default().to_string();
        made.push(Bundle {
            offer: Offer {
                id: id.clone(),
                text: format!(
                    "{:?} ({}): {}; then press {}",
                    meta.title,
                    meta.kind,
                    mapped.join(", "),
                    name_of(press)
                ),
            },
            args: json!({
                "item": meta.item,
                "kind": meta.kind,
                "fields": fields.iter().map(|(l, r)| json!([l, r])).collect::<Vec<_>>(),
                "press": press,
                "snapshot": shown["snapshot"],
                "suggestion": id,
            }),
            title: meta.title.clone(),
            confirm: secret && meta.kind != "login",
        });
    }
    made
}

/// What a `needs_fields` carries: the items that fit, with the fields each
/// holds; the page's fields and buttons as refs; what the page asks for;
/// its other buttons and links; latch's suggestions.
fn fields_stop(seen: &Seen) -> Value {
    let controls = super::super::fill::controls_of(&json!({"controls": seen.view["controls"]}));
    json!({
        "items": seen.fill["items"],
        "candidates": suggest::candidates(&controls),
        "page_offers": seen.fill["page_offers"],
        "alternatives": seen.fill["alternatives"],
        "suggestions": seen.fill["suggestions"],
    })
}

/// The first text field on the page, for a `needs_text` with no better one.
fn first_field(view: &Value) -> Option<Value> {
    view["elements"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|e| e["kind"] == "text" && e["secret"] != true && e["disabled"] != true)
        .map(|e| json!({"ref": e["ref"], "role": e["role"], "name": e["name"]}))
}

/// The form with the most fields on the page, if it has two or more: its
/// fields and buttons, as refs for a `fill` step.
fn form_of(elements: &[&Value]) -> Vec<Value> {
    let field = |e: &Value| e["kind"] == "text" || e["kind"] == "select";
    let mut counts: BTreeMap<(String, i64), usize> = BTreeMap::new();
    for e in elements.iter().filter(|e| field(e)) {
        if let Some(form) = e["form"].as_i64() {
            *counts
                .entry((e["frame"].as_str().unwrap_or("f0").to_string(), form))
                .or_default() += 1;
        }
    }
    let Some(((frame, form), _)) = counts
        .into_iter()
        .filter(|(_, count)| *count >= 2)
        .max_by_key(|(_, count)| *count)
    else {
        return Vec::new();
    };
    elements
        .iter()
        .filter(|e| e["hidden"] != true && e["disabled"] != true)
        .filter(|e| field(e) || e["role"] == "button")
        .filter(|e| {
            e["frame"].as_str().unwrap_or("f0") == frame && e["form"].as_i64() == Some(form)
        })
        .map(|e| {
            let mut candidate = json!({"ref": e["ref"], "role": e["role"], "name": e["name"]});
            if e["secret"] == true {
                candidate["secret"] = json!(true);
            }
            candidate
        })
        .collect()
}

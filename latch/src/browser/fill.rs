//! The host's half of `login`, `fill` and `fill-code`: the vault's gate,
//! and the order of the worker's fill steps (`core/fill.rs`).
//!
//! 1. **Gate.** Any session may use any item; where it may go is the
//!    site rule's question (step 3). A `fill` that includes a secret field
//!    (card number, CVV, …, as the classifier says) needs `--press`: a
//!    secret leaves the page before the agent is back. It is also
//!    confirmed by the agent before any value is released (`steps.rs`,
//!    [`releases_secret`]); this runs once it is.
//! 2. **Pause and prepare.** The browser is held: every other call, in
//!    any session on the profile, gets `session_paused`. The worker
//!    reports where each value would go.
//! 3. **Match.** Each field frame's origin and the top-level origin must
//!    pass the item kind's site rule (`vault::sites`). Only then is a value
//!    released, one at a time, into the worker's private pipe.
//! 4. **Submit, clean up, resume.** latch presses the submit (or `--press`)
//!    button, waits for the values to leave the page, and lets the agent
//!    back in. Cleanup it cannot confirm leaves the session paused.
//!
//! A login whose page takes the username alone (the first of two pages)
//! goes on within the same pause: after its button, latch waits for the
//! password field, and prepares, matches and submits there as in steps 2
//! to 4, with the origins checked again on that page.
//!
//! The result says `submitted`, never "logged in". The audit records the
//! item, its kind, the fields by label, both origins (a second page's under
//! `password_step`), whether agent code ever ran on those sites in the
//! profile (`agent_code_before_fill`), and the outcome: never a value.

use std::sync::atomic::Ordering;

use serde_json::{Map, Value, json};

use super::host::{Browsers, Entry, paused, take_audit};
use super::session::Session;
use crate::failure::{Exit, Failure};
use crate::vault::item::Item;
use crate::vault::store::Backend;
use crate::vault::{classify, dates, sites, totp};

/// Whether a `fill` releases a secret field, and so needs the agent's
/// confirmation first: the item's title if it does. A malformed fill is
/// refused here, as `run` would refuse it.
pub fn releases_secret(browsers: &Browsers, args: &Value) -> Result<Option<String>, Failure> {
    let id = args["item"]
        .as_str()
        .ok_or_else(|| Failure::usage("bad_arguments", "name the vault item with --item".into()))?;
    let item = browsers.vault.with(|store| {
        store
            .item(id)
            .ok_or_else(|| Failure::usage("no_such_item", format!("there is no vault item {id}")))
    })?;
    let plan = Plan::of("fill", &item, args)?;
    let secret = plan
        .fields
        .iter()
        .any(|(label, _)| classify::is_secret(&item.raw, label));
    Ok(secret.then(|| item.meta().title))
}

pub async fn run(
    browsers: &Browsers,
    entry: &Entry,
    command: &str,
    args: &Value,
    audit: &mut Map<String, Value>,
) -> Result<Value, Failure> {
    let id = args["item"]
        .as_str()
        .ok_or_else(|| Failure::usage("bad_arguments", "name the vault item with --item".into()))?;
    audit.insert("item".into(), json!(id));
    let item = browsers.vault.with(|store| {
        store
            .item(id)
            .ok_or_else(|| Failure::usage("no_such_item", format!("there is no vault item {id}")))
    })?;
    let meta = item.meta();
    audit.insert("kind".into(), json!(meta.kind));
    let plan = Plan::of(command, &item, args)?;
    audit.insert("fields".into(), json!(plan.labels()));
    if entry.runtime.filling.swap(true, Ordering::SeqCst) {
        return Err(paused());
    }
    entry.runtime.pauses.fetch_add(1, Ordering::SeqCst);
    let mut session = match entry.agent_session().await {
        Ok(session) => session,
        Err(failure) => {
            entry.runtime.filling.store(false, Ordering::SeqCst);
            return Err(failure);
        }
    };
    let result = fill(browsers, &mut session, &item, &plan, args, audit).await;
    entry.runtime.filling.store(false, Ordering::SeqCst);
    result
}

/// What the agent asked to fill, checked against the item.
struct Plan {
    mode: &'static str,
    /// Item label (or `code`), and the refs it goes into (none: latch picks).
    fields: Vec<(String, Vec<String>)>,
    submit: Option<String>,
}

impl Plan {
    fn labels(&self) -> Vec<&str> {
        self.fields
            .iter()
            .map(|(label, _)| label.as_str())
            .collect()
    }

    fn of(command: &str, item: &Item, args: &Value) -> Result<Plan, Failure> {
        let kind = item.meta().kind;
        let usage = |message: &str| Failure::usage("bad_arguments", message.to_string());
        let named = |key: &str| args[key].as_str().map(|r| vec![r.to_string()]);
        match command {
            "login" => {
                if kind != "login" {
                    return Err(usage(
                        "login takes a login item; fill takes cards, identities and notes",
                    ));
                }
                let mut fields = Vec::new();
                for key in ["username", "password"] {
                    if let Some(refs) = named(key) {
                        fields.push((key.to_string(), refs));
                    }
                }
                Ok(Plan {
                    mode: "login",
                    fields,
                    submit: args["submit"].as_str().map(str::to_string),
                })
            }
            "fill_code" => {
                if !item.meta().totp {
                    return Err(usage(
                        "fill-code takes a login item with an authenticator key",
                    ));
                }
                Ok(Plan {
                    mode: "code",
                    fields: named("field")
                        .map(|refs| vec![("code".to_string(), refs)])
                        .unwrap_or_default(),
                    submit: args["submit"].as_str().map(str::to_string),
                })
            }
            _ => {
                if kind == "login" {
                    return Err(usage(
                        "fill takes cards, identities and notes; a login uses login",
                    ));
                }
                let mut fields = Vec::new();
                for pair in args["fields"].as_array().into_iter().flatten() {
                    let label = pair[0].as_str().unwrap_or_default().to_string();
                    let refs: Vec<String> = pair[1]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|r| r.as_str().map(str::to_string))
                        .collect();
                    if classify::read(&item.raw, &label).is_none() {
                        return Err(Failure::usage(
                            "no_such_field",
                            format!(
                                "item {} has no field {label:?}; vault list shows its fields",
                                item.id
                            ),
                        ));
                    }
                    if kind == "note"
                        && label != "notes"
                        && !item
                            .raw
                            .fields
                            .iter()
                            .any(|f| f.name.as_deref() == Some(label.as_str()))
                    {
                        return Err(usage("a note fills its notes or its own custom fields"));
                    }
                    if refs.is_empty() {
                        return Err(usage("each --field names its refs: LABEL=eN[,eN…]"));
                    }
                    fields.push((label, refs));
                }
                if fields.is_empty() {
                    return Err(usage("fill needs --field LABEL=eN for each field"));
                }
                let press = args["press"].as_str().map(str::to_string);
                let secret = fields
                    .iter()
                    .any(|(label, _)| classify::is_secret(&item.raw, label));
                if secret && press.is_none() {
                    return Err(Failure::new(
                        Exit::Denied,
                        "press_required",
                        "that fill includes a secret field, which must leave the page before you are back: add --press eN, the button that submits it".into(),
                    ));
                }
                Ok(Plan {
                    mode: "fields",
                    fields,
                    submit: press,
                })
            }
        }
    }
}

/// Sends one fill step to the worker; its audit joins the call's.
async fn step(
    session: &mut Session,
    call: Value,
    audit: &mut Map<String, Value>,
) -> Result<Value, Failure> {
    match session.call(call).await {
        Ok(reply) => {
            if let Some(worker) = reply.audit.as_object() {
                for (key, value) in worker {
                    audit.entry(key.clone()).or_insert_with(|| value.clone());
                }
            }
            Ok(reply.result)
        }
        Err(mut failure) => {
            take_audit(&mut failure, audit);
            Err(failure)
        }
    }
}

async fn fill(
    browsers: &Browsers,
    session: &mut Session,
    item: &Item,
    plan: &Plan,
    args: &Value,
    audit: &mut Map<String, Value>,
) -> Result<Value, Failure> {
    let prepare = json!({
        "op": "prepare_fill",
        "snapshot": args["snapshot"],
        "mode": plan.mode,
        "fields": plan.fields,
        "submit": plan.submit,
    });
    let prepared = match step(session, prepare, audit).await {
        Ok(prepared) => prepared,
        Err(failure) => return Err(resume_after(session, failure, audit).await),
    };
    // The fields latch picked, when the agent named none.
    let mut filled = labels(&prepared);
    audit.insert("fields".into(), json!(filled));
    if let Err(failure) = release_and_set(browsers, session, item, &prepared, audit).await {
        return Err(stop(session, failure, audit).await);
    }
    let submitted = prepared["submit"].as_str().map(str::to_string);
    if let Some(submit) = &submitted {
        audit.insert("pressed".into(), json!(submit));
        let pressed = json!({"op": "fill_submit", "target": submit, "guard": args["guard"]});
        if let Err(failure) = step(session, pressed, audit).await {
            return Err(stop(session, failure, audit).await);
        }
        if plan.mode == "login" && plan.fields.is_empty() && filled == ["username"] {
            let mut second = Map::new();
            let outcome = password_step(browsers, session, item, &mut second).await;
            audit.insert("password_step".into(), Value::Object(second));
            match outcome {
                Ok(labels) => {
                    for label in labels {
                        if !filled.contains(&label) {
                            filled.push(label);
                        }
                    }
                }
                Err(failure) => return Err(stop(session, failure, audit).await),
            }
        }
        let cleaned = step(session, json!({"op": "fill_cleanup"}), audit).await;
        if let Ok(cleaned) = &cleaned {
            audit.insert(
                "cleared_by_latch".into(),
                cleaned["cleared_by_latch"].clone(),
            );
        }
        if !cleaned.as_ref().is_ok_and(|c| c["clean"] == true) {
            // The values may still be on the page: the agent stays out.
            return Err(stuck("submitted"));
        }
    }
    let mut result = step(session, json!({"op": "fill_resume"}), audit).await?;
    if let Some(object) = result.as_object_mut() {
        object.insert("item".into(), json!(item.id));
        object.insert("filled".into(), json!(filled));
        object.insert("submitted".into(), json!(submitted.is_some()));
    }
    Ok(result)
}

/// The fields a prepared step picked, by label.
fn labels(prepared: &Value) -> Vec<String> {
    prepared["fields"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|field| field["field"].as_str().map(str::to_string))
        .collect()
}

/// The second page of a two-page login: once its password field shows,
/// latch picks the form there, checks its origins, fills and submits.
/// Answers the labels filled.
async fn password_step(
    browsers: &Browsers,
    session: &mut Session,
    item: &Item,
    audit: &mut Map<String, Value>,
) -> Result<Vec<String>, Failure> {
    let prepare = json!({"op": "prepare_fill", "mode": "login", "password_step": true});
    let prepared = step(session, prepare, audit).await?;
    let filled = labels(&prepared);
    audit.insert("fields".into(), json!(filled));
    release_and_set(browsers, session, item, &prepared, audit).await?;
    let submit = prepared["submit"].as_str().unwrap_or_default();
    audit.insert("pressed".into(), json!(submit));
    step(
        session,
        json!({"op": "fill_submit", "target": submit}),
        audit,
    )
    .await?;
    Ok(filled)
}

/// After a fill that stopped with values possibly written: clears them, and
/// lets the agent back in only once that is confirmed; otherwise the
/// session stays paused, and that is the answer.
async fn stop(session: &mut Session, failure: Failure, audit: &mut Map<String, Value>) -> Failure {
    let cleaned = step(session, json!({"op": "fill_cleanup"}), audit).await;
    if cleaned.as_ref().is_ok_and(|c| c["clean"] == true) {
        return resume_after(session, failure, audit).await;
    }
    audit.insert("stopped_by".into(), json!(failure.code));
    stuck(&failure.code)
}

/// The session stays paused: values may still be on the page.
fn stuck(why: &str) -> Failure {
    Failure::failed(
        "session_paused",
        format!(
            "the fill stopped ({why}) and latch could not confirm the filled values left the page; the session stays paused (close it to end it)"
        ),
    )
}

/// Lets the agent back in after a fill that stopped, then reports why. The
/// worker lets no one in while a value any fill put on the page may still
/// be there (an earlier fill's cleanup that was never confirmed): then the
/// session stays paused, and that is the answer.
async fn resume_after(
    session: &mut Session,
    failure: Failure,
    audit: &mut Map<String, Value>,
) -> Failure {
    match step(session, json!({"op": "fill_resume"}), audit).await {
        Err(refused) if refused.code == "session_paused" => {
            audit.insert("stopped_by".into(), json!(failure.code));
            stuck(&failure.code)
        }
        _ => failure,
    }
}

/// Checks every field's origins, then releases each value and sets it,
/// reshaped and split as its boxes need.
async fn release_and_set(
    browsers: &Browsers,
    session: &mut Session,
    item: &Item,
    prepared: &Value,
    audit: &mut Map<String, Value>,
) -> Result<(), Failure> {
    let meta = item.meta();
    let top = prepared["top_origin"].as_str().unwrap_or("opaque");
    audit.insert("top_origin".into(), json!(top));
    let mut frame_origins: Vec<String> = Vec::new();
    let fields = prepared["fields"].as_array().cloned().unwrap_or_default();
    for field in &fields {
        for target in field["boxes"].as_array().into_iter().flatten() {
            let origin = target["origin"].as_str().unwrap_or("opaque");
            if !frame_origins.iter().any(|o| o == origin) {
                frame_origins.push(origin.to_string());
            }
            audit.insert("frame_origins".into(), json!(frame_origins));
            let (seen, page) = (browsers.origin_as(origin), browsers.origin_as(top));
            if let Err(why) = sites::allows(&meta, &seen, &page) {
                let mut failure = Failure::new(
                    Exit::Denied,
                    "origin_mismatch",
                    format!("nothing was filled: {why}"),
                );
                failure.details = json!({"reason": why});
                return Err(failure);
            }
            if meta.kind == "note" && target["page_secret"] == true {
                return Err(Failure::new(
                    Exit::Denied,
                    "secret_field",
                    "a note fills only fields the page does not mark secret".into(),
                ));
            }
            if target["kind"] != "text" && target["kind"] != "select" {
                return Err(Failure::usage(
                    "wrong_target",
                    "a value goes into a text field or a list".into(),
                ));
            }
        }
    }
    // Agent code that ran on these sites, in any session, may have left a
    // service worker or stored script to see what is filled now.
    let touched: Vec<String> = frame_origins
        .iter()
        .map(String::as_str)
        .chain([top])
        .filter_map(sites::site_of)
        .collect();
    audit.insert(
        "agent_code_before_fill".into(),
        json!(session.runtime.agent_code_on(&touched)),
    );
    for field in &fields {
        let label = field["field"].as_str().unwrap_or_default();
        let boxes: Vec<Value> = field["boxes"].as_array().cloned().unwrap_or_default();
        let value = release(browsers, item, label)?;
        let value = shaped(label, value, &boxes[0])?;
        let pieces = split(&value, &boxes).ok_or_else(|| {
            Failure::failed(
                "split_mismatch",
                format!(
                    "{label} does not split into those {} boxes; nothing was filled",
                    boxes.len()
                ),
            )
        })?;
        for (target, piece) in boxes.iter().zip(pieces) {
            let set = json!({"op": "set_value", "target": target["ref"], "value": piece});
            let answer = step(session, set, audit).await?;
            if answer["held"] != true {
                return Err(Failure::failed(
                    "needs_owner",
                    "the page would not keep the value latch set; this site needs the owner (handoff)".into(),
                ));
            }
        }
    }
    Ok(())
}

/// One value out of the vault: a field, or for `code` the current TOTP
/// code, computed here from the stored key.
fn release(browsers: &Browsers, item: &Item, label: &str) -> Result<String, Failure> {
    if label == "code" && item.meta().kind == "login" {
        let key = browsers
            .vault
            .with(|store| store.release(&item.id, classify::TOTP))?;
        let params = totp::Params::parse(&key).map_err(|why| Failure::failed("bad_item", why))?;
        return Ok(params.current());
    }
    browsers.vault.with(|store| store.release(&item.id, label))
}

/// A date reshaped to the field's own format (its placeholder or length,
/// or the label's usual shape); a two-digit year box gets two digits.
fn shaped(label: &str, value: String, field: &Value) -> Result<String, Failure> {
    if label == "expiry year" && field["max_length"].as_u64() == Some(2) && value.len() == 4 {
        return Ok(value[2..].to_string());
    }
    let Some(default) = dates::default_shape(label) else {
        return Ok(value);
    };
    let placeholder = field["placeholder"].as_str().unwrap_or("");
    let max_length = field["max_length"].as_u64().map(|n| n as u32);
    let pattern =
        dates::pattern_of(placeholder, max_length, label).unwrap_or_else(|| default.to_string());
    if pattern.is_empty() {
        return Ok(value);
    }
    dates::format(&value, &pattern).map_err(|_| {
        Failure::failed(
            "bad_item",
            format!("the stored {label} is not a date; correct it in the vault"),
        )
    })
}

/// A value across its boxes: whole into one; by each box's length when
/// those add up; one character each when the counts match. Otherwise none.
fn split(value: &str, boxes: &[Value]) -> Option<Vec<String>> {
    if boxes.len() == 1 {
        return Some(vec![value.to_string()]);
    }
    let chars: Vec<char> = value.chars().collect();
    let lengths: Option<Vec<usize>> = boxes
        .iter()
        .map(|b| b["max_length"].as_u64().map(|n| n as usize))
        .collect();
    if let Some(lengths) = lengths
        && lengths.iter().sum::<usize>() == chars.len()
    {
        let mut at = 0;
        return Some(
            lengths
                .iter()
                .map(|n| {
                    let piece: String = chars[at..at + n].iter().collect();
                    at += n;
                    piece
                })
                .collect(),
        );
    }
    (chars.len() == boxes.len()).then(|| chars.iter().map(|c| c.to_string()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_split_by_box_length_or_one_character_each() {
        let four = json!({"max_length": 4});
        assert_eq!(
            split(
                "4111111111111111",
                &[four.clone(), four.clone(), four.clone(), four]
            )
            .unwrap(),
            ["4111", "1111", "1111", "1111"]
        );
        let unbounded = json!({"max_length": null});
        assert_eq!(
            split("123456", &vec![unbounded.clone(); 6]).unwrap(),
            ["1", "2", "3", "4", "5", "6"]
        );
        assert!(split("12345", &vec![unbounded; 6]).is_none());
        assert_eq!(split("whole", &[json!({})]).unwrap(), ["whole"]);
    }

    #[test]
    fn dates_take_the_fields_own_shape() {
        let field = |placeholder: &str, max: Option<u64>| json!({"placeholder": placeholder, "max_length": max});
        assert_eq!(
            shaped("expiry", "2031-04".into(), &field("MM / YY", None)).unwrap(),
            "04 / 31"
        );
        assert_eq!(
            shaped("expiry", "2031-04".into(), &field("", Some(7))).unwrap(),
            "04/2031"
        );
        assert_eq!(
            shaped("expiry", "2031-04".into(), &field("", None)).unwrap(),
            "04/31"
        );
        assert_eq!(
            shaped("date of birth", "1984-11-09".into(), &field("", None)).unwrap(),
            "1984-11-09"
        );
        assert_eq!(
            shaped(
                "date of birth",
                "1984-11-09".into(),
                &field("mm/dd/yyyy", None)
            )
            .unwrap(),
            "11/09/1984"
        );
        assert_eq!(
            shaped("expiry year", "2031".into(), &field("YY", Some(2))).unwrap(),
            "31"
        );
        assert_eq!(
            shaped("number", "4111".into(), &field("", None)).unwrap(),
            "4111"
        );
    }
}

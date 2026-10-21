//! The host's half of `fill`, for every vault kind: what a fill may use on
//! a page (`offered`, for `observe` and plans), a suggestion taken by its
//! id (`resolve`), and the fill itself, run in the worker's steps
//! (`core/fill.rs`):
//!
//! 1. **Gate.** Any session may use any item; where it may go is the
//!    site rule's question (step 4). A mapping that includes a secret (a
//!    password, a code, a card or identity secret) needs a press button: a
//!    secret leaves the page before the agent is back. One that releases a
//!    card or identity secret is also confirmed by the agent before any
//!    value is released (`steps.rs`, [`releases_secret`]); this runs once
//!    it is.
//! 2. **Pause and prepare.** The browser is held: every other call, in
//!    any session on the profile, gets `session_paused`. The worker
//!    reports what the page's markup says of each mapped element.
//! 3. **Check.** Each element must take its item field (`suggest::check`):
//!    what the markup contradicts is refused `bad_target`, before any value
//!    leaves the vault.
//! 4. **Match.** Each field frame's origin and the top-level origin must
//!    pass the item kind's site rule (`vault::sites`). Only then is a value
//!    released, one at a time, into the worker's private pipe.
//! 5. **Submit, clean up, resume.** latch presses the mapping's button,
//!    waits for the values to leave the page, and lets the agent back in.
//!    Cleanup it cannot confirm leaves the session paused.
//!
//! A login whose mapping has no password (an identifier stage) goes on
//! within the same pause: after its button, latch reads the next stage
//! itself, and fills the password there only when exactly one field takes
//! it, its form is no registration and has a single submit button, and
//! the origins pass again. Anything else ends the fill after cleanup with
//! `needs_fields`, the new stage's fields shown.
//!
//! The result says `submitted`, never "logged in" or "paid", with each
//! field's outcome. The audit records the item, its revision, the mapping
//! by ref, where it came from (a suggestion's id, or the agent), the
//! stages, the origins, any `hint_mismatch`, whether agent code ever ran on
//! those sites in the profile (`agent_code_before_fill`), and the outcome:
//! never a value.

use std::sync::atomic::Ordering;

use serde_json::{Map, Value, json};

use super::host::{Browsers, Entry, paused, take_audit};
use super::session::Session;
use super::suggest::{self, Control};
use crate::failure::{Exit, Failure};
use crate::vault::item::{Item, Meta};
use crate::vault::store::Backend;
use crate::vault::{classify, dates, sites, totp};

/// Whether a fill releases a card or identity secret, and so needs the
/// agent's confirmation first: the item's title if it does. A malformed
/// fill is refused here, as `run` would refuse it. A login's password and
/// code need no confirmation: they go only to the login's own site.
pub fn releases_secret(browsers: &Browsers, args: &Value) -> Result<Option<String>, Failure> {
    let item = item_of(browsers, args)?;
    let plan = Plan::of(&item, args)?;
    let meta = item.meta();
    let secret = meta.kind != "login"
        && plan
            .fields
            .iter()
            .any(|(label, _)| suggest::secret_field(&meta, label));
    Ok(secret.then_some(meta.title))
}

fn item_of(browsers: &Browsers, args: &Value) -> Result<Item, Failure> {
    let id = args["item"]
        .as_str()
        .ok_or_else(|| Failure::usage("bad_arguments", "name the vault item with --item".into()))?;
    browsers.vault.with(|store| {
        store
            .item(id)
            .ok_or_else(|| Failure::usage("no_such_item", format!("there is no vault item {id}")))
    })
}

/// What a fill could use on a page, for `observe`, a `needs_fields` and a
/// plan's pass: the vault items that fit it (by their kind's site rule for
/// a frame holding a field, under the top-level page), each with the fields
/// it holds and never a value; what the page asks for; latch's suggestions
/// for those items; and the page's other buttons and links in its forms.
pub fn offered(browsers: &Browsers, inventory: &Value) -> Value {
    let controls = controls_of(inventory);
    let snapshot = inventory["snapshot"].as_str().unwrap_or_default();
    let top = browsers.origin_as(inventory["top_origin"].as_str().unwrap_or("opaque"));
    let mut frames: Vec<String> = Vec::new();
    for control in controls.iter().filter(|c| c.takes_value()) {
        let origin = browsers.origin_as(&control.origin);
        if !frames.contains(&origin) {
            frames.push(origin);
        }
    }
    let mut items = Vec::new();
    let mut suggestions = Vec::new();
    for meta in browsers.vault.list_if_present() {
        if !frames
            .iter()
            .any(|frame| sites::allows(&meta, frame, &top).is_ok())
        {
            continue;
        }
        for made in suggest::suggest(&meta, snapshot, &controls) {
            let allowed = made
                .frames
                .iter()
                .all(|frame| sites::allows(&meta, &browsers.origin_as(frame), &top).is_ok());
            if allowed {
                let mut shown = made.to_json();
                shown["frames"] = json!(
                    made.frames
                        .iter()
                        .map(|f| browsers.origin_as(f))
                        .collect::<Vec<_>>()
                );
                suggestions.push(shown);
            }
        }
        items.push(holding(&meta));
    }
    let presses: Vec<&str> = suggestions
        .iter()
        .filter_map(|s| s["press"].as_str())
        .collect();
    let alternatives = suggest::alternatives(&controls, &presses);
    json!({
        "snapshot": snapshot,
        "items": items,
        "page_offers": suggest::offers(&controls),
        "suggestions": suggestions,
        "alternatives": alternatives,
    })
}

/// An item as a fill may use it: its fields, each present, and secret or
/// derived where so. Never a value, and no username.
pub fn holding(meta: &Meta) -> Value {
    let mut fields = Vec::new();
    for field in meta.fields.iter().filter(|f| !f.alias) {
        if field.label == classify::TOTP {
            continue;
        }
        let mut shown = json!({"id": field.label, "present": true});
        if suggest::secret_field(meta, &field.label) {
            shown["secret"] = json!(true);
        }
        fields.push(shown);
    }
    if meta.kind == "login" && meta.totp {
        fields.push(json!({"id": "otp", "present": true, "secret": true, "derived": "totp"}));
    }
    json!({"item": meta.item, "title": meta.title, "kind": meta.kind,
           "revision": meta.revision, "fields": fields})
}

/// The elements of a worker's inventory, as a fill sees them.
pub fn controls_of(inventory: &Value) -> Vec<Control> {
    inventory["controls"]
        .as_array()
        .into_iter()
        .flatten()
        .map(Control::of)
        .collect()
}

/// A fill that takes suggestion `suggested`, as the explicit mapping it
/// stands for: recomputed on its snapshot now, for the item as it is. A
/// suggestion that no longer comes out the same is refused `stale_snapshot`,
/// with the page as it is now and its suggestions; latch never maps anew
/// on its own. Arguments with no suggestion come back as they are.
pub async fn resolve(browsers: &Browsers, entry: &Entry, args: &Value) -> Result<Value, Failure> {
    let Some(id) = args["suggested"].as_str() else {
        return Ok(args.clone());
    };
    let item = item_of(browsers, args)?;
    let meta = item.meta();
    let inventory = worker(
        entry,
        json!({"op": "fill_inventory", "snapshot": args["snapshot"]}),
    )
    .await;
    let found = inventory.as_ref().ok().and_then(|inventory| {
        let snapshot = inventory["snapshot"].as_str().unwrap_or_default();
        suggest::suggest(&meta, snapshot, &controls_of(inventory))
            .into_iter()
            .find(|s| s.id == id)
    });
    let Some(found) = found else {
        let mut failure = Failure::failed(
            "stale_snapshot",
            format!(
                "suggestion {id} no longer fits the page or item {} as they are now; nothing was released. Use one of the fresh suggestions, or map the fields yourself",
                meta.item
            ),
        );
        if let Ok((page, _)) = worker_with(entry, json!({"op": "observe"})).await {
            let fresh = worker(
                entry,
                json!({"op": "fill_inventory", "snapshot": page["snapshot"]}),
            )
            .await
            .map(|inventory| offered(browsers, &inventory))
            .unwrap_or(Value::Null);
            failure.details = json!({"page": page, "fill": fresh});
        }
        return Err(failure);
    };
    let mut resolved = args.clone();
    if let Some(object) = resolved.as_object_mut() {
        object.remove("suggested");
        object.insert(
            "fields".into(),
            json!(
                found
                    .fields
                    .iter()
                    .map(|(label, refs)| json!([label, refs]))
                    .collect::<Vec<_>>()
            ),
        );
        object.insert("snapshot".into(), json!(found.snapshot));
        object.insert("suggestion".into(), json!(found.id));
        if object.get("press").is_none_or(Value::is_null) {
            object.insert("press".into(), json!(found.press));
        }
    }
    Ok(resolved)
}

/// One worker call outside a fill's pause: refused while another fill
/// holds the browser.
async fn worker(entry: &Entry, call: Value) -> Result<Value, Failure> {
    worker_with(entry, call).await.map(|(result, _)| result)
}

async fn worker_with(entry: &Entry, call: Value) -> Result<(Value, Value), Failure> {
    if entry.runtime.filling.load(Ordering::SeqCst) {
        return Err(paused());
    }
    let mut session = entry.agent_session().await?;
    let reply = session.call(call).await?;
    Ok((reply.result, reply.audit))
}

/// The mapping as the agent will see it confirmed: for each item field,
/// each element's ref, its name on the page and its markup's evidence; the
/// origin of each frame a value would go into, as the site rules read it;
/// after checking each element takes its field (`bad_target` otherwise).
pub async fn preview(
    browsers: &Browsers,
    entry: &Entry,
    args: &Value,
) -> Result<(Vec<Value>, Vec<String>), Failure> {
    let item = item_of(browsers, args)?;
    let meta = item.meta();
    let plan = Plan::of(&item, args)?;
    let inventory = worker(
        entry,
        json!({"op": "fill_inventory", "snapshot": args["snapshot"]}),
    )
    .await?;
    let controls = controls_of(&inventory);
    let mut shown = Vec::new();
    let mut frames: Vec<String> = Vec::new();
    for (label, refs) in &plan.fields {
        for reference in refs {
            let Some(control) = controls.iter().find(|c| &c.reference == reference) else {
                return Err(bad_target(
                    label,
                    &format!("{reference} is not on that page"),
                ));
            };
            let mismatch =
                suggest::check(meta.kind, label, control).map_err(|why| bad_target(label, &why))?;
            let mut entry = json!({"field": label, "ref": reference, "name": control.name});
            if let Some(token) = control.hint.field.as_deref() {
                entry["evidence"] = json!(format!("autocomplete={token}"));
            }
            if let Some(token) = mismatch {
                entry["hint_mismatch"] = json!(token);
            }
            shown.push(entry);
            let origin = browsers.origin_as(&control.origin);
            if !frames.contains(&origin) {
                frames.push(origin);
            }
        }
    }
    Ok((shown, frames))
}

fn bad_target(label: &str, why: &str) -> Failure {
    let mut failure = Failure::new(
        Exit::Denied,
        "bad_target",
        format!("nothing was released: {label} can't go there: {why}"),
    );
    failure.details = json!({"field": label, "reason": why});
    failure
}

pub async fn run(
    browsers: &Browsers,
    entry: &Entry,
    args: &Value,
    audit: &mut Map<String, Value>,
) -> Result<Value, Failure> {
    let args = &resolve(browsers, entry, args).await?;
    let item = item_of(browsers, args)?;
    audit.insert("item".into(), json!(item.id));
    let meta = item.meta();
    audit.insert("kind".into(), json!(meta.kind));
    audit.insert("revision".into(), json!(meta.revision));
    audit.insert(
        "source".into(),
        args.get("suggestion")
            .cloned()
            .unwrap_or_else(|| json!("agent")),
    );
    let plan = Plan::of(&item, args)?;
    audit.insert("mapping".into(), plan.mapping());
    if let Some(confirmed) = args["confirmed"]["revision"].as_u64()
        && confirmed != meta.revision
    {
        return Err(Failure::failed(
            "context_changed",
            "the item changed after the fill was confirmed; nothing was released".into(),
        ));
    }
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
    /// Item field, and the refs it goes into.
    fields: Vec<(String, Vec<String>)>,
    press: Option<String>,
}

impl Plan {
    fn mapping(&self) -> Value {
        Value::Object(
            self.fields
                .iter()
                .map(|(label, refs)| (label.clone(), json!(refs)))
                .collect(),
        )
    }

    fn of(item: &Item, args: &Value) -> Result<Plan, Failure> {
        let meta = item.meta();
        let usage = |message: &str| Failure::usage("bad_arguments", message.to_string());
        let mut fields: Vec<(String, Vec<String>)> = Vec::new();
        for pair in args["fields"].as_array().into_iter().flatten() {
            let label = pair[0].as_str().unwrap_or_default().to_string();
            let refs: Vec<String> = pair[1]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|r| r.as_str().map(str::to_string))
                .collect();
            let known = match meta.kind {
                "login" if label == classify::TOTP => {
                    return Err(usage(
                        "a login's authenticator key never leaves the vault: map `otp`, the code computed from it",
                    ));
                }
                "login" if label == "otp" => meta.totp,
                _ => classify::read(&item.raw, &label).is_some(),
            };
            if !known {
                return Err(Failure::usage(
                    "no_such_field",
                    format!(
                        "item {} has no field {label:?}; observe lists the fields each item holds",
                        item.id
                    ),
                ));
            }
            if meta.kind == "note"
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
                return Err(usage("each --field names its refs: FIELD=eN[,eN…]"));
            }
            if fields.iter().any(|(other, _)| *other == label) {
                return Err(usage(&format!("{label} is mapped twice")));
            }
            for reference in &refs {
                let taken = fields.iter().any(|(_, others)| others.contains(reference))
                    || refs.iter().filter(|r| *r == reference).count() > 1;
                if taken {
                    return Err(bad_target(
                        &label,
                        &format!("{reference} already takes another field"),
                    ));
                }
            }
            fields.push((label, refs));
        }
        if fields.is_empty() {
            return Err(usage(
                "fill needs --field FIELD=eN for each field, or --suggested S",
            ));
        }
        let press = args["press"].as_str().map(str::to_string);
        let secret = fields
            .iter()
            .any(|(label, _)| suggest::secret_field(&meta, label));
        if secret && press.is_none() {
            return Err(Failure::new(
                Exit::Denied,
                "press_required",
                "that fill includes a secret, which must leave the page before you are back: add --press eN, the button that submits it".into(),
            ));
        }
        Ok(Plan { fields, press })
    }

    /// An identifier stage: a login's username (by any of its names) mapped
    /// alone, with its button, for an item that holds a password.
    fn identifier_stage(&self, meta: &Meta) -> bool {
        meta.kind == "login"
            && self.press.is_some()
            && matches!(self.fields.as_slice(),
                        [(label, _)] if matches!(label.as_str(), "username" | "email" | "login" | "user"))
            && meta.fields.iter().any(|f| f.label == "password")
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
    let meta = item.meta();
    let prepare = json!({
        "op": "prepare_fill",
        "snapshot": args["snapshot"],
        "fields": plan.fields,
        "submit": plan.press,
    });
    let prepared = match step(session, prepare, audit).await {
        Ok(prepared) => prepared,
        Err(failure) => return Err(resume_after(session, failure, audit).await),
    };
    let mut outcomes = match checked(&meta, &prepared) {
        Ok(outcomes) => outcomes,
        Err(failure) => return Err(resume_after(session, failure, audit).await),
    };
    if let Some(sites) = args["confirmed"]["frames"].as_array() {
        let now = frame_sites(browsers, &prepared);
        if now.iter().any(|site| !sites.contains(&json!(site))) {
            let failure = Failure::failed(
                "context_changed",
                "a field's frame is on another site than the one confirmed; nothing was released"
                    .into(),
            );
            return Err(resume_after(session, failure, audit).await);
        }
    }
    if let Err(failure) = release_and_set(browsers, session, item, &prepared, audit).await {
        return Err(stop(session, failure, audit).await);
    }
    let mut stages = 1;
    if let Some(press) = &plan.press {
        audit.insert("pressed".into(), json!(press));
        let pressed = json!({"op": "fill_submit", "target": press, "guard": args["guard"]});
        if let Err(failure) = step(session, pressed, audit).await {
            return Err(stop(session, failure, audit).await);
        }
        if plan.identifier_stage(&meta) {
            let mut second = Map::new();
            let outcome = next_stage(browsers, session, item, &mut second).await;
            match outcome {
                Ok(Stage::Filled(more)) => {
                    stages = 2;
                    outcomes.extend(more);
                    audit.insert("second_stage".into(), Value::Object(second));
                }
                Ok(Stage::Ended(ended)) => {
                    audit.insert("second_stage".into(), Value::Object(second));
                    return Err(stage_ended(browsers, session, ended, audit).await);
                }
                Err(failure) => {
                    audit.insert("second_stage".into(), Value::Object(second));
                    return Err(stop(session, failure, audit).await);
                }
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
    audit.insert("stages".into(), json!(stages));
    let mismatches: Vec<&Value> = outcomes
        .iter()
        .filter(|o| o.get("hint_mismatch").is_some())
        .collect();
    if !mismatches.is_empty() {
        audit.insert("hint_mismatch".into(), json!(mismatches));
    }
    let mut result = step(session, json!({"op": "fill_resume"}), audit).await?;
    if let Some(object) = result.as_object_mut() {
        object.insert("item".into(), json!(item.id));
        object.insert("fields".into(), json!(outcomes));
        object.insert("stages".into(), json!(stages));
        object.insert("submitted".into(), json!(plan.press.is_some()));
    }
    Ok(result)
}

/// Each prepared element against the item field it takes: the field's
/// outcome (`filled` once set), or `bad_target` for the first that the
/// page's markup contradicts.
fn checked(meta: &Meta, prepared: &Value) -> Result<Vec<Value>, Failure> {
    let mut outcomes = Vec::new();
    for field in prepared["fields"].as_array().into_iter().flatten() {
        let label = field["field"].as_str().unwrap_or_default();
        for target in field["boxes"].as_array().into_iter().flatten() {
            let control = Control::of(target);
            let mismatch = suggest::check(meta.kind, label, &control)
                .map_err(|why| bad_target(label, &why))?;
            let mut outcome =
                json!({"field": label, "ref": control.reference, "outcome": "filled"});
            if let Some(token) = mismatch {
                outcome["hint_mismatch"] = json!(token);
            }
            outcomes.push(outcome);
        }
    }
    Ok(outcomes)
}

/// The site of each frame a prepared fill would write into.
fn frame_sites(browsers: &Browsers, prepared: &Value) -> Vec<String> {
    let mut sites: Vec<String> = Vec::new();
    for field in prepared["fields"].as_array().into_iter().flatten() {
        for target in field["boxes"].as_array().into_iter().flatten() {
            let origin = browsers.origin_as(target["origin"].as_str().unwrap_or("opaque"));
            if !sites.contains(&origin) {
                sites.push(origin);
            }
        }
    }
    sites
}

/// How a login's second stage went.
enum Stage {
    /// The password went in and its button was pressed: its outcome.
    Filled(Vec<Value>),
    /// The stage was no password stage latch fills by itself: why, and the
    /// stage as latch read it.
    Ended(String),
}

/// A login's next stage, after its identifier stage: the password goes in
/// only when exactly one field takes it, on a form that is no registration,
/// with that form's single submit button, and the origins pass again.
async fn next_stage(
    browsers: &Browsers,
    session: &mut Session,
    item: &Item,
    audit: &mut Map<String, Value>,
) -> Result<Stage, Failure> {
    let meta = item.meta();
    let stage = step(session, json!({"op": "next_stage"}), audit).await?;
    let controls = controls_of(&stage);
    let offers = suggest::offers(&controls);
    audit.insert("page_offers".into(), json!(offers));
    let passwords: Vec<&Control> = controls
        .iter()
        .filter(|c| suggest::check("login", "password", c).is_ok())
        .collect();
    let [password] = passwords.as_slice() else {
        return Ok(Stage::Ended(if passwords.is_empty() {
            format!(
                "the next stage asks for no password ({})",
                described(&offers)
            )
        } else {
            "the next stage has more than one password field".into()
        }));
    };
    let form: Vec<&Control> = controls
        .iter()
        .filter(|c| c.form.is_some() && c.form == password.form)
        .collect();
    if password.form.is_none()
        || form
            .iter()
            .any(|c| c.hint.field.as_deref() == Some("new-password"))
        || form.iter().filter(|c| c.kind == "password").count() > 1
    {
        return Ok(Stage::Ended(
            "the next stage's password field is in no form, or in one that looks like a registration".into(),
        ));
    }
    let submits: Vec<&&Control> = form
        .iter()
        .filter(|c| c.submit && c.unusable.is_none())
        .collect();
    let [submit] = submits.as_slice() else {
        return Ok(Stage::Ended(
            "the next stage's form has no single submit button".into(),
        ));
    };
    let prepare = json!({
        "op": "prepare_fill",
        "snapshot": stage["snapshot"],
        "fields": [["password", [password.reference]]],
        "submit": submit.reference,
    });
    let prepared = step(session, prepare, audit).await?;
    let outcomes = checked(&meta, &prepared)?;
    release_and_set(browsers, session, item, &prepared, audit).await?;
    audit.insert("pressed".into(), json!(submit.reference));
    step(
        session,
        json!({"op": "fill_submit", "target": submit.reference}),
        audit,
    )
    .await?;
    Ok(Stage::Filled(outcomes))
}

fn described(offers: &[&str]) -> String {
    if offers.is_empty() {
        "no sign-in field latch can tell".into()
    } else {
        format!("it asks for: {}", offers.join(", "))
    }
}

/// A second stage latch does not fill by itself: the first stage's values
/// are cleaned up, the agent is let back in, and the stage comes back as
/// the agent's own page with what a fill could use there.
async fn stage_ended(
    browsers: &Browsers,
    session: &mut Session,
    why: String,
    audit: &mut Map<String, Value>,
) -> Failure {
    let ended = || {
        Failure::failed(
            "needs_fields",
            format!(
                "latch submitted the first stage and stopped: {why}. Map this stage's fields in a new fill, or hand off to the owner"
            ),
        )
    };
    let mut failure = ended();
    let stopped = stop(session, ended(), audit).await;
    if stopped.code != "needs_fields" {
        return stopped;
    }
    let page = step(session, json!({"op": "observe"}), &mut Map::new()).await;
    let mut details = json!({"first_stage": "submitted"});
    if let Ok(page) = page {
        let inventory = step(
            session,
            json!({"op": "fill_inventory", "snapshot": page["snapshot"]}),
            &mut Map::new(),
        )
        .await;
        if let Ok(inventory) = inventory {
            let controls = controls_of(&inventory);
            details["candidates"] = json!(suggest::candidates(&controls));
            details["fill"] = offered(browsers, &inventory);
        }
        details["page"] = page;
    }
    failure.details = details;
    failure
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
            let set = json!({"op": "set_value", "target": target["ref"], "value": piece,
                             "secret": suggest::secret_field(&meta, label)});
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

/// One value out of the vault: a field, or for a login's `otp` the current
/// TOTP code, computed here from the stored key.
fn release(browsers: &Browsers, item: &Item, label: &str) -> Result<String, Failure> {
    if label == "otp" && item.meta().kind == "login" {
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
    use crate::vault::item::{Custom, LOGIN, Raw};

    #[test]
    fn only_a_username_fill_goes_on_to_the_password_stage() {
        let mut raw = Raw::from_labels(
            LOGIN,
            "Shop",
            &[
                ("username".into(), "ada@shop.example".into()),
                ("password".into(), "made-up".into()),
            ],
        )
        .unwrap();
        raw.fields.push(Custom {
            name: Some("account number".into()),
            value: Some("1234".into()),
            kind: 0,
            linked_id: None,
        });
        let meta = Item {
            id: "v1".into(),
            revision: 1,
            sites: vec!["shop.example".into()],
            other_top_sites: vec![],
            raw,
        }
        .meta();
        let plan = |label: &str| Plan {
            fields: vec![(label.to_string(), vec!["e1".to_string()])],
            press: Some("e2".into()),
        };
        assert!(plan("username").identifier_stage(&meta));
        assert!(plan("email").identifier_stage(&meta));
        assert!(!plan("account number").identifier_stage(&meta));
        assert!(!plan("otp").identifier_stage(&meta));
    }

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

//! The browser's half of a vault `fill`. The host runs the steps in order,
//! holding the session the whole time:
//!
//! 1. `PrepareFill` pauses the session (every agent call now gets
//!    `session_paused`) and reports, for each ref of the mapping, what the
//!    page's markup says of it (`__latch.fillTarget`) and where a value
//!    would go: the field frame's effective origin (`self.origin` in
//!    latch's isolated world), the top-level origin, and the document's
//!    token. latch never picks a field here: the mapping names every one;
//! 2. the host checks the mapping and those origins and releases values,
//!    one `SetValue` each: the driver re-checks element, document and
//!    origin and sets the value in one step, never by typing;
//! 3. `FillSubmit` clicks the mapping's press button; after a login's
//!    identifier stage, `NextStage` waits for the page's next stage and
//!    reads it privately, and the host may run steps 1 to 3 there once;
//! 4. `FillCleanup` waits until each document a value went into is
//!    replaced or its filled fields are empty, clearing them itself when
//!    the page keeps them; if that cannot be confirmed the session stays
//!    paused;
//! 5. `FillResume` lets the agent back in, never while an unconfirmed
//!    cleanup, of this fill or an earlier one, stands.
//!
//! `FillInventory` reports every element of a snapshot as a fill sees it
//! (`__latch.controls`), for the host's suggestions and checks; it pauses
//! nothing. No step answers with a value. While a filled value may still be
//! on a page (a fill without a press), in whichever tab, `eval` and
//! screenshots are refused.

use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::super::driver::{Driver, Frame};
use super::{Core, stale};
use crate::failure::{Failure, code};

/// How long a submitted form has to replace or empty its document.
const CLEANUP: Duration = Duration::from_secs(15);
/// How long latch watches fields it emptied before calling them empty.
const SETTLED: Duration = Duration::from_millis(500);
/// How long a page that shows an error after the press has to move on
/// before latch takes the error as its answer.
const ERROR_SETTLED: Duration = Duration::from_secs(3);
/// How long a login's next stage has to show after its identifier stage.
const NEXT_STAGE: Duration = Duration::from_secs(15);
/// How long a changed page must stay unchanged to count as the next stage,
/// when it shows no password, code or passkey field.
const STAGE_SETTLED: Duration = Duration::from_secs(1);

#[derive(Deserialize, Debug)]
pub struct Prepare {
    pub snapshot: Option<String>,
    /// Item field, and the refs it goes into; several refs split one value
    /// across boxes.
    pub fields: Vec<(String, Vec<String>)>,
    pub submit: Option<String>,
}

/// A snapshot's frame, with its refs and their indexes in that frame.
type FrameRefs = (Frame, Vec<(String, usize)>);

/// One field a value goes into.
struct Target {
    reference: String,
    frame: Frame,
    snapshot: String,
    index: usize,
    origin: String,
    token: String,
}

/// A filled field and the value it should still hold.
#[derive(Deserialize, Debug)]
pub struct Check {
    target: String,
    value: String,
    #[serde(default)]
    formatted: bool,
}

/// What changed around a confirmed fill's press button between the
/// confirmation and the press: its amounts, the button's name, the rest.
fn changed_since(guard: &super::Guard, now: &Value) -> String {
    let lines: Vec<String> = now["lines"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|l| l.as_str().map(str::to_string))
        .collect();
    let (before, after) = (amounts(&guard.lines), amounts(&lines));
    let name = now["element"]["name"].as_str().unwrap_or_default();
    let mut said = Vec::new();
    if before != after {
        said.push(format!("amounts {} → {}", list(&before), list(&after)));
    }
    if name == guard.name {
        said.push(format!("button {:?} unchanged", guard.name));
    } else {
        said.push(format!("button {:?} → {:?}", guard.name, name));
    }
    if before == after && lines != guard.lines {
        said.push("other text around the button changed".into());
    }
    format!("since the confirmation: {}", said.join("; "))
}

/// The amounts `lines` show: words with a currency sign or code and a digit.
fn amounts(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .flat_map(|line| line.split_whitespace())
        .map(|word| word.trim_matches(|c: char| !c.is_alphanumeric() && !"$€£¥".contains(c)))
        .filter(|word| {
            (word.chars().any(|c| "$€£¥".contains(c))
                || ["USD", "EUR", "GBP"].iter().any(|code| word.contains(code)))
                && word.chars().any(|c| c.is_ascii_digit())
        })
        .map(str::to_string)
        .collect()
}

fn list(words: &[String]) -> String {
    if words.is_empty() {
        "none".into()
    } else {
        words.join(", ")
    }
}

/// How many of a value's last characters a fill types as keys: every one
/// of a password, a code or a card number.
const TYPED_KEYS: usize = 64;

#[derive(Default)]
pub struct State {
    /// The snapshot the fill was prepared on.
    snapshot: Option<String>,
    targets: Vec<Target>,
    /// Frames a value went into, with their tab, to check for leftovers
    /// and clean.
    filled: Vec<(String, Frame)>,
    /// A cleanup could not confirm the values left the page; until a later
    /// one does, nobody is let back in.
    unclean: bool,
    /// The page's fields as the fill was prepared: a later stage differs.
    stage: Option<String>,
    /// Secrets filled into the page, which no GET request may carry while
    /// they may be on it (`Driver::guard`).
    guarded: Vec<String>,
    /// The errors the page showed before the press: not the press's.
    errors_before: Vec<Value>,
}

/// The filled frames still open, in the active tab and in others.
struct Filled {
    here: Vec<(String, Frame)>,
    elsewhere: Vec<(String, Frame)>,
}

impl<D: Driver> Core<D> {
    pub(super) fn prepare_fill(
        &mut self,
        prepare: Prepare,
        audit: &mut Map<String, Value>,
    ) -> Result<Value, Failure> {
        self.no_dialog()?;
        self.paused = true;
        self.fill.targets.clear();
        self.page(audit)?;
        let snapshot = match prepare.snapshot.as_deref() {
            Some(id) => self.snapshot(id)?.id.clone(),
            None => self.snapshots.back().ok_or_else(stale)?.id.clone(),
        };
        if prepare.fields.is_empty() {
            return Err(Failure::new(
                code::bad_arguments,
                "a fill names each field it fills".into(),
            ));
        }
        let main = self.main_frame()?;
        let top = self
            .driver
            .run(&main, "self.origin === 'null' ? 'opaque' : self.origin")?;
        let top = top.as_str().unwrap_or("opaque").to_string();
        let mut targets = Vec::new();
        let mut reported = Vec::new();
        for (key, refs) in &prepare.fields {
            let mut boxes = Vec::new();
            for reference in refs {
                let (id, position) = self.bound(Some(&snapshot), reference)?;
                let (frame, index) = self.reference(&id, position)?;
                let mut info = self
                    .driver
                    .run(&frame, &format!("__latch.fillTarget({id:?}, {index})"))?;
                if info["gone"] == true {
                    return Err(stale());
                }
                let token = info["token"].as_str().unwrap_or_default().to_string();
                if let Some(object) = info.as_object_mut() {
                    object.remove("token");
                    object.insert("ref".into(), json!(reference));
                    object.insert("frame".into(), json!(frame.id));
                }
                targets.push(Target {
                    reference: reference.clone(),
                    frame,
                    snapshot: id,
                    index,
                    origin: info["origin"].as_str().unwrap_or("opaque").to_string(),
                    token,
                });
                boxes.push(info);
            }
            reported.push(json!({"field": key, "boxes": boxes}));
        }
        if let Some(submit) = &prepare.submit {
            self.bound(Some(&snapshot), submit)?;
        }
        self.fill.targets = targets;
        self.fill.snapshot = Some(snapshot.clone());
        self.fill.stage = self.stage_now().ok().map(|(mark, _)| mark);
        Ok(
            json!({"snapshot": snapshot, "top_origin": top, "fields": reported, "submit": prepare.submit}),
        )
    }

    /// Every element of a snapshot (default: the latest shown) as a fill
    /// sees it, by ref, with its frame and that frame's origin; and the
    /// top-level origin.
    pub(super) fn fill_inventory(&mut self, snapshot: Option<&str>) -> Result<Value, Failure> {
        let snapshot = match snapshot {
            Some(id) => self.snapshot(id)?.id.clone(),
            None => self.last_shown.clone().ok_or_else(stale)?,
        };
        self.inventory(&snapshot)
    }

    pub(super) fn inventory(&mut self, snapshot: &str) -> Result<Value, Failure> {
        let main = self.main_frame()?;
        let top = self
            .driver
            .run(&main, "self.origin === 'null' ? 'opaque' : self.origin")?;
        let mut controls = Vec::new();
        for (frame, refs) in self.frames_of(snapshot)? {
            let Some(found) = self.in_frame(&frame, &format!("__latch.controls({snapshot:?})"))?
            else {
                continue;
            };
            let listed = found["controls"].as_array().cloned().unwrap_or_default();
            for (reference, index) in refs {
                let Some(mut control) = listed.get(index).cloned() else {
                    continue;
                };
                if control["gone"] == true {
                    continue;
                }
                control["ref"] = json!(reference);
                control["frame"] = json!(frame.id);
                control["origin"] = found["origin"].clone();
                controls.push(control);
            }
        }
        Ok(json!({"snapshot": snapshot, "top_origin": top, "controls": controls}))
    }

    /// The page's fields now, as one mark (each frame's document and its
    /// rendered fields' markup), and whether one asks for a password, a
    /// code or a passkey.
    fn stage_now(&mut self) -> Result<(String, bool), Failure> {
        let mut mark = String::new();
        let mut asks = false;
        for frame in self.driver.frames()? {
            let Ok(found) = self.driver.run(&frame, "__latch.stageMark()") else {
                continue;
            };
            mark.push_str(&frame.document);
            mark.push(':');
            mark.push_str(found["mark"].as_str().unwrap_or_default());
            mark.push('\n');
            asks |= found["asks"] == true;
        }
        Ok((mark, asks))
    }

    /// After a login's identifier stage: waits for the next stage (the
    /// page's fields changed, and show a password, code or passkey field,
    /// or stayed as they are for a moment), then reads it privately.
    pub(super) fn next_stage(&mut self) -> Result<Value, Failure> {
        let before = self.fill.stage.clone().unwrap_or_default();
        let deadline = Instant::now() + NEXT_STAGE;
        let mut last: Option<(String, Instant)> = None;
        // What the page did meanwhile, a refusal of the first stage's
        // submission among it.
        let mut events = Vec::new();
        loop {
            events.extend(self.settle().unwrap_or_default());
            if let Ok((mark, asks)) = self.stage_now()
                && mark != before
            {
                if asks {
                    break;
                }
                match &last {
                    Some((seen, since)) if *seen == mark => {
                        if since.elapsed() >= STAGE_SETTLED {
                            break;
                        }
                    }
                    _ => last = Some((mark, Instant::now())),
                }
            }
            if Instant::now() >= deadline || self.driver.dialog_open() {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        // latch's own reading of the new stage, not the agent's.
        self.quiet = true;
        let reading = self.read(None)?;
        let mut stage = self.inventory(&reading.snapshot)?;
        stage["events"] = json!(events);
        Ok(stage)
    }

    /// `expression`'s answer in one of a snapshot's frames, or `None` when
    /// the frame went away since (pages drop frames all the time); one that
    /// went away held no field to fill.
    fn in_frame(&mut self, frame: &Frame, expression: &str) -> Result<Option<Value>, Failure> {
        match self.driver.run(frame, expression) {
            Ok(found) => Ok(Some(found)),
            Err(failure) if failure.code == "stale_snapshot" => Ok(None),
            Err(failure) => Err(failure),
        }
    }

    /// Each frame of a snapshot, with its refs and their in-frame indexes.
    fn frames_of(&self, snapshot: &str) -> Result<Vec<FrameRefs>, Failure> {
        let mut frames: Vec<FrameRefs> = Vec::new();
        for entry in &self.snapshot(snapshot)?.refs {
            let reference = (format!("e{}", entry.number), entry.index);
            match frames.iter_mut().find(|(f, _)| f.id == entry.frame.id) {
                Some((_, refs)) => refs.push(reference),
                None => frames.push((entry.frame.clone(), vec![reference])),
            }
        }
        Ok(frames)
    }

    /// One value into one prepared field, after checking its element,
    /// document and origin again. Answers whether the field holds it.
    pub(super) fn set_value(
        &mut self,
        target: &str,
        value: &str,
        secret: bool,
        formatted: bool,
    ) -> Result<Value, Failure> {
        let Some(at) = self.fill.targets.iter().position(|t| t.reference == target) else {
            return Err(Failure::new(
                code::bad_request,
                format!("{target} was not prepared for this fill"),
            ));
        };
        let frames = self.driver.frames()?;
        let target = &self.fill.targets[at];
        let refused =
            |why: &str| Failure::new(code::fill_refused, format!("nothing was filled: {why}"));
        if !frames
            .iter()
            .any(|f| f.id == target.frame.id && f.document == target.frame.document)
        {
            return Err(refused("the document was replaced"));
        }
        // A secret is guarded before it reaches the page, as stored and as
        // a single-line field holds it, without its line breaks; and each
        // part of it the field holds as the keys arrive.
        if secret {
            let unbroken = value.replace(['\r', '\n'], "");
            let parts: Vec<String> = [value, unbroken.as_str()]
                .iter()
                .flat_map(|form| {
                    form.char_indices()
                        .map(|(at, ch)| form[..at + ch.len_utf8()].to_string())
                })
                .collect();
            let mut added = false;
            for form in parts {
                if !form.is_empty() && !self.fill.guarded.contains(&form) {
                    self.fill.guarded.push(form);
                    added = true;
                }
            }
            if added {
                self.driver.guard(&self.fill.guarded)?;
            }
        }
        let (frame, snapshot, index, origin, token) = (
            target.frame.clone(),
            target.snapshot.clone(),
            target.index,
            target.origin.clone(),
            target.token.clone(),
        );
        // The frame is registered before the first key: a fill that fails
        // part-way through the typing is cleaned up there too.
        if !self
            .fill
            .filled
            .iter()
            .any(|(_, f)| f.id == frame.id && f.document == frame.document)
        {
            let tab = self.driver.active_tab();
            self.fill.filled.push((tab, frame.clone()));
        }
        let answer =
            self.type_value(&frame, &snapshot, index, value, &origin, &token, formatted)?;
        if let Some(why) = answer["refused"].as_str() {
            return Err(refused(why));
        }
        Ok(json!({"held": answer["held"] == true,
                  "dropped_breaks": answer["dropped_breaks"] == true,
                  "page": answer["page"]}))
    }

    /// Puts `value` into the field as a person types it: a text field
    /// gets its last keys (all of them, for any value under
    /// [`TYPED_KEYS`]) as the engine's own key presses, the head before
    /// them set in one step; any other field, or a value with a line break
    /// or a tab, which keys would turn into a submit or a move, is set in
    /// one step. Answers the reader's `held`, or its refusal.
    #[allow(clippy::too_many_arguments)]
    fn type_value(
        &mut self,
        frame: &Frame,
        snapshot: &str,
        index: usize,
        value: &str,
        origin: &str,
        token: &str,
        formatted: bool,
    ) -> Result<Value, Failure> {
        let at = value
            .char_indices()
            .rev()
            .nth(TYPED_KEYS - 1)
            .map_or(0, |(at, _)| at);
        let (head, tail) = value.split_at(at);
        let start = if tail.contains(['\r', '\n', '\t']) {
            json!({"assign": true})
        } else {
            self.driver.call(
                frame,
                "function (s, i, h, o, t) { return __latch.typeStart(s, i, h, o, t); }",
                vec![
                    json!(snapshot),
                    json!(index),
                    json!(head),
                    json!(origin),
                    json!(token),
                ],
            )?
        };
        if start["refused"].is_string() {
            return Ok(start);
        }
        // A field whose frame can't hold the page's focus gets no keys:
        // they would go to another frame.
        if start["assign"] == true || !self.driver.holds_focus(frame)? {
            return self.driver.call(
                frame,
                "function (s, i, v, o, t, f) { return __latch.fill(s, i, v, o, t, f); }",
                vec![
                    json!(snapshot),
                    json!(index),
                    json!(value),
                    json!(origin),
                    json!(token),
                    json!(formatted),
                ],
            );
        }
        let mut caret = start["caret"].as_u64().unwrap_or(0);
        for key in tail.chars() {
            let ready = self.driver.call(
                frame,
                "function (s, i, c, t) { return __latch.typeKey(s, i, c, t); }",
                vec![json!(snapshot), json!(index), json!(caret), json!(token)],
            )?;
            if ready["refused"].is_string() {
                return Ok(ready);
            }
            if !self.driver.holds_focus(frame)? {
                return Ok(json!({"refused": "the field's frame lost the focus"}));
            }
            self.driver.type_text(&key.to_string())?;
            caret = ready["caret"].as_u64().unwrap_or(0) + key.len_utf16() as u64;
        }
        self.driver.call(
            frame,
            "function (s, i, v, t, f) { return __latch.typeEnd(s, i, v, t, f); }",
            vec![
                json!(snapshot),
                json!(index),
                json!(value),
                json!(token),
                json!(formatted),
            ],
        )
    }

    /// Clicks the submit or `--press` button, as an agent's click would.
    pub(super) fn fill_submit(
        &mut self,
        target: &str,
        guard: Option<&super::Guard>,
    ) -> Result<Value, Failure> {
        let prepared = self.fill.snapshot.clone();
        // A page changes as a card goes in (the brand, a hint, a total
        // with tax): the press goes on, and the answer says what changed.
        let mut since = None;
        if let Some(guard) = guard {
            let now = self.review(prepared.as_deref(), target, guard.enter, &mut Map::new())?;
            if super::fingerprint(&now) != guard.fingerprint {
                since = Some(changed_since(guard, &now));
            }
        }
        let (snapshot, position) = self.bound(prepared.as_deref(), target)?;
        let at = self.locate(&snapshot, position)?;
        self.fill.errors_before = self.page_errors();
        self.driver.click(at.x, at.y)?;
        let events = self.settle()?;
        self.no_secret_sent()?;
        let mut answer = json!({"pressed": target, "events": events});
        if let Some(since) = since {
            answer["since_confirmation"] = json!(since);
        }
        Ok(answer)
    }

    /// Waits until every document a value went into is replaced or empty of
    /// filled values, clearing them itself once the wait is over. Answers
    /// `clean: false` when that cannot be confirmed; the session then stays
    /// paused.
    /// The page's own errors, read as it waits (`page_errors`): a page that
    /// shows one keeps the values, and latch stops waiting.
    pub(super) fn fill_cleanup(&mut self, now: bool) -> Result<Value, Failure> {
        // Unconfirmed until this cleanup positively confirms: an error on
        // the way leaves the session paused too.
        self.fill.unclean = true;
        let deadline = Instant::now() + if now { Duration::ZERO } else { CLEANUP };
        // What the page did while latch waited, a refused request among it.
        let mut events = Vec::new();
        // A page that shows an error after the press is given a moment to
        // move on all the same (a payment still processing), then latch
        // stops waiting for it.
        let mut shown_since: Option<Instant> = None;
        let errors = loop {
            if self.holding().ok() == Some(0) {
                self.fill.unclean = false;
                return Ok(
                    json!({"clean": true, "cleared_by_latch": false, "events": events,
                                 "page_errors": self.new_errors()}),
                );
            }
            let errors = self.new_errors();
            if !errors.is_empty() {
                shown_since.get_or_insert_with(Instant::now);
            }
            let settled = shown_since.is_some_and(|since| since.elapsed() >= ERROR_SETTLED);
            if settled || Instant::now() >= deadline || self.driver.dialog_open() {
                break errors;
            }
            std::thread::sleep(Duration::from_millis(200));
            events.extend(self.settle().unwrap_or_default());
        };
        for (_, frame) in self.live_filled()?.here {
            let _ = self.driver.run(&frame, "__latch.clearFilled()");
        }
        // Clean only on a positive answer, given after the page had a moment
        // to put a value back: a frame that cannot be read may still hold it.
        std::thread::sleep(SETTLED);
        let clean = self.holding().ok() == Some(0);
        self.fill.unclean = !clean;
        Ok(
            json!({"clean": clean, "cleared_by_latch": clean, "events": events,
                  "page_errors": errors}),
        )
    }

    /// The errors the page shows now that it didn't before the press.
    fn new_errors(&mut self) -> Vec<Value> {
        let before = std::mem::take(&mut self.fill.errors_before);
        let errors = self
            .page_errors()
            .into_iter()
            .filter(|error| !before.contains(error))
            .collect();
        self.fill.errors_before = before;
        errors
    }

    /// Every error the page's frames show now.
    fn page_errors(&mut self) -> Vec<Value> {
        let mut errors = Vec::new();
        for frame in self.driver.frames().unwrap_or_default() {
            if let Ok(found) = self.driver.run(&frame, "__latch.pageErrors()") {
                errors.extend(found.as_array().cloned().unwrap_or_default());
            }
        }
        errors
    }

    /// Whether each filled field still holds its value: `[{target, held,
    /// page}]`, never a value.
    pub(super) fn fill_held(&mut self, checks: &[Check]) -> Result<Value, Failure> {
        let mut answers = Vec::new();
        for check in checks {
            let Some(target) = self
                .fill
                .targets
                .iter()
                .find(|t| t.reference == check.target)
            else {
                continue;
            };
            let (frame, snapshot, index) =
                (target.frame.clone(), target.snapshot.clone(), target.index);
            let answer = self.driver.call(
                &frame,
                "function (s, i, v, f) { return __latch.heldNow(s, i, v, f); }",
                vec![
                    json!(snapshot),
                    json!(index),
                    json!(check.value),
                    json!(check.formatted),
                ],
            )?;
            answers.push(
                json!({"target": check.target, "held": answer["held"] == true,
                                "page": answer["page"]}),
            );
        }
        Ok(json!(answers))
    }

    /// The fields the form requires that hold nothing, the fill's own
    /// aside: `[{ref, name}]`, on the snapshot the fill was prepared on.
    pub(super) fn fill_missing(&mut self) -> Result<Value, Failure> {
        let snapshot = self.fill.snapshot.clone().ok_or_else(stale)?;
        let inventory = self.inventory(&snapshot)?;
        let ours: Vec<String> = self
            .fill
            .targets
            .iter()
            .map(|t| t.reference.clone())
            .collect();
        let missing: Vec<Value> = inventory["controls"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|c| c["required"] == true && c["empty"] == true)
            .filter(|c| c["hidden"] != true && c["disabled"] != true)
            .filter(|c| !ours.iter().any(|r| c["ref"] == r.as_str()))
            .map(|c| json!({"ref": c["ref"], "name": c["name"]}))
            .collect();
        Ok(json!(missing))
    }

    /// Lets the agent back in, unless a cleanup could not confirm the values
    /// left the page: that keeps the session paused through every later
    /// fill, until one's cleanup confirms the page holds none.
    pub(super) fn fill_resume(&mut self, audit: &mut Map<String, Value>) -> Result<Value, Failure> {
        if self.fill.unclean {
            return Err(Failure::new(
                code::session_paused,
                "a value latch filled may still be on the page; the session stays paused (close it to end it)".into(),
            ));
        }
        self.fill.targets.clear();
        self.paused = false;
        let before = Map::new();
        let mut result = Map::new();
        let events = self.settle()?;
        self.after(&before, events, &mut result)?;
        result.extend(self.page(audit)?);
        Ok(Value::Object(result))
    }

    /// Filled frames whose document may still be the one filled: in the
    /// active tab, those whose document is unchanged; in another tab, every
    /// one while that tab is open, since only the active tab's frames can
    /// be read.
    fn live_filled(&mut self) -> Result<Filled, Failure> {
        let active = self.driver.active_tab();
        let tabs = self.driver.tabs()?;
        let frames = self.driver.frames()?;
        self.fill.filled.retain(|(tab, f)| {
            if *tab == active {
                frames
                    .iter()
                    .any(|now| now.id == f.id && now.document == f.document)
            } else {
                tabs.iter().any(|open| open.tab == *tab)
            }
        });
        let (here, elsewhere) = self
            .fill
            .filled
            .iter()
            .cloned()
            .partition(|(tab, _)| *tab == active);
        Ok(Filled { here, elsewhere })
    }

    /// How many filled fields still hold a value; an error when a filled
    /// document cannot be read, as in a tab that is not the active one.
    fn holding(&mut self) -> Result<u64, Failure> {
        let filled = self.live_filled()?;
        if !filled.elsewhere.is_empty() {
            return Err(Failure::new(
                code::engine_error,
                "a tab latch filled is not the active one".into(),
            ));
        }
        let mut total = 0;
        for (_, frame) in filled.here {
            // A frame whose document is still the filled one but cannot be
            // read is not known to be empty.
            let count = self.driver.run(&frame, "__latch.holding()")?;
            total += count.as_u64().ok_or_else(|| {
                Failure::new(
                    code::engine_error,
                    "latch could not count the filled fields".into(),
                )
            })?;
        }
        // Nothing filled is left anywhere: no request needs stopping.
        if total == 0 && !self.fill.guarded.is_empty() {
            self.fill.guarded.clear();
            self.driver.guard(&[])?;
        }
        Ok(total)
    }

    /// Refused when the page just tried to send a filled secret in a GET
    /// request's address: the browser stopped it, and nothing was sent.
    pub(super) fn no_secret_sent(&mut self) -> Result<(), Failure> {
        if self.driver.guard_stopped() {
            return Err(Failure::new(
                code::get_form,
                "the page tried to send a value latch filled from the vault in an address (a GET request); latch stopped it, and nothing was sent".into(),
            ));
        }
        Ok(())
    }

    /// `eval` and screenshots are refused while a filled value may still
    /// be on a page of the session, in any of its tabs.
    pub(super) fn no_value_on_page(&mut self, what: &str) -> Result<(), Failure> {
        if self.holding().ok() != Some(0) {
            return Err(Failure::new(
                code::secret_on_page,
                format!(
                    "a value latch filled from the vault may still be on a page of this session; {what} waits until it is gone (or its tab is closed)"
                ),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::super::super::driver::{Download, Event, Tab};
    use super::super::Call;
    use super::*;

    /// A browser that can no longer list its frames, with a dialog up: a
    /// cleanup on it fails at once.
    struct Broken;

    impl Driver for Broken {
        fn limit(&mut self, _: Option<std::time::Instant>) {}
        fn front(&mut self) -> Result<(), Failure> {
            unreachable!()
        }
        fn tabs(&mut self) -> Result<Vec<Tab>, Failure> {
            Ok(vec![Tab {
                tab: "t1".into(),
                url: "https://shop.example/checkout".into(),
                title: "Checkout".into(),
                opener: None,
                active: true,
            }])
        }
        fn frames(&mut self) -> Result<Vec<Frame>, Failure> {
            Err(Failure::new(
                code::engine_error,
                "the frames are gone".into(),
            ))
        }
        fn settle(&mut self) -> Result<Vec<Event>, Failure> {
            Ok(Vec::new())
        }
        fn dialog_open(&self) -> bool {
            true
        }
        fn active_tab(&self) -> String {
            "t1".into()
        }
        fn capabilities(&self) -> Vec<String> {
            Vec::new()
        }
        fn new_tab(&mut self, _: Option<&str>) -> Result<String, Failure> {
            unreachable!()
        }
        fn switch_tab(&mut self, _: &str) -> Result<(), Failure> {
            unreachable!()
        }
        fn close_tab(&mut self, _: &str) -> Result<(), Failure> {
            unreachable!()
        }
        fn navigate(&mut self, _: &str) -> Result<(), Failure> {
            unreachable!()
        }
        fn back(&mut self) -> Result<bool, Failure> {
            unreachable!()
        }
        fn run(&mut self, _: &Frame, _: &str) -> Result<Value, Failure> {
            unreachable!()
        }
        fn call(&mut self, _: &Frame, _: &str, _: Vec<Value>) -> Result<Value, Failure> {
            unreachable!()
        }
        fn holds_focus(&mut self, _: &Frame) -> Result<bool, Failure> {
            unreachable!()
        }
        fn reaches(&mut self, _: &Frame, _: f64, _: f64) -> Result<bool, Failure> {
            unreachable!()
        }
        fn frame_offset(&mut self, _: &Frame) -> Result<(f64, f64), Failure> {
            unreachable!()
        }
        fn click(&mut self, _: f64, _: f64) -> Result<(), Failure> {
            unreachable!()
        }
        fn type_text(&mut self, _: &str) -> Result<(), Failure> {
            unreachable!()
        }
        fn select_all(&mut self) -> Result<(), Failure> {
            unreachable!()
        }
        fn press(&mut self, _: &str) -> Result<(), Failure> {
            unreachable!()
        }
        fn wheel(&mut self, _: f64, _: f64, _: f64) -> Result<(), Failure> {
            unreachable!()
        }
        fn answer_dialog(&mut self, _: bool, _: Option<&str>) -> Result<(), Failure> {
            unreachable!()
        }
        fn eval(&mut self, _: &str) -> Result<Value, Failure> {
            unreachable!()
        }
        fn set_files(&mut self, _: &Frame, _: &str, _: &[PathBuf]) -> Result<(), Failure> {
            unreachable!()
        }
        fn upload_folder(&self) -> PathBuf {
            unreachable!()
        }
        fn downloads(&mut self) -> Result<Vec<Download>, Failure> {
            unreachable!()
        }
        fn download_file(&mut self, _: &str) -> Result<(Download, PathBuf), Failure> {
            unreachable!()
        }
        fn screenshot(&mut self) -> Result<(Value, Vec<u8>), Failure> {
            unreachable!()
        }
        fn close(&mut self) -> Value {
            unreachable!()
        }
    }

    #[test]
    fn a_cleanup_that_fails_keeps_every_later_resume_out() {
        let mut core = Core::new(Broken);
        core.paused = true;
        core.fill.filled.push((
            "t1".into(),
            Frame {
                id: "f1".into(),
                parent: None,
                document: "d1".into(),
            },
        ));
        let cleaned = core
            .call(Call::FillCleanup { now: false }, None)
            .err()
            .unwrap();
        assert_eq!(cleaned.code, "engine_error");
        // As after a later fill that failed before releasing anything.
        let resumed = core.call(Call::FillResume, None).err().unwrap();
        assert_eq!(resumed.code, "session_paused");
        assert!(core.paused);
    }
}

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
//! on a page (a fill without a press), in whichever tab, `eval` is refused
//! and a screenshot shows the filled fields blank; a secret among them is
//! emptied once the time its fill gave is up (`clear_due`).

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
/// How long latch waits to empty a filled secret again when its tab is
/// not the active one, and so can't be read.
const CLEAR_AGAIN: Duration = Duration::from_secs(30);
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
    /// When the secrets a fill left on the page are emptied.
    clear_at: Option<Instant>,
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
        // An earlier fill's deadline stands only while its fields still
        // hold something: one whose page went has nothing left to empty.
        if self.holding().ok() == Some(0) {
            self.fill.clear_at = None;
        }
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
        if let Some(guard) = guard {
            let now = self.review(prepared.as_deref(), target, guard.enter, &mut Map::new())?;
            if super::fingerprint(&now) != guard.fingerprint {
                return Err(Failure::new(
                    code::context_changed,
                    "the button, or the lines around it, changed after the fill was confirmed; it was not pressed".into(),
                ));
            }
        }
        let (snapshot, position) = self.bound(prepared.as_deref(), target)?;
        let at = self.locate(&snapshot, position)?;
        self.driver.click(at.x, at.y)?;
        let events = self.settle()?;
        self.no_secret_sent()?;
        Ok(json!({"pressed": target, "events": events}))
    }

    /// Waits until every document a value went into is replaced or empty of
    /// filled values, clearing them itself once the wait is over. Answers
    /// `clean: false` when that cannot be confirmed; the session then stays
    /// paused.
    pub(super) fn fill_cleanup(&mut self) -> Result<Value, Failure> {
        // Unconfirmed until this cleanup positively confirms: an error on
        // the way leaves the session paused too.
        self.fill.unclean = true;
        let deadline = Instant::now() + CLEANUP;
        // What the page did while latch waited, a refused request among it.
        let mut events = Vec::new();
        loop {
            if self.holding().ok() == Some(0) {
                self.fill.unclean = false;
                return Ok(json!({"clean": true, "cleared_by_latch": false, "events": events}));
            }
            if Instant::now() >= deadline || self.driver.dialog_open() {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
            events.extend(self.settle().unwrap_or_default());
        }
        for (_, frame) in self.live_filled()?.here {
            let _ = self.driver.run(&frame, "__latch.clearFilled()");
        }
        // Clean only on a positive answer, given after the page had a moment
        // to put a value back: a frame that cannot be read may still hold it.
        std::thread::sleep(SETTLED);
        let clean = self.holding().ok() == Some(0);
        self.fill.unclean = !clean;
        Ok(json!({"clean": clean, "cleared_by_latch": clean, "events": events}))
    }

    /// Lets the agent back in, unless a cleanup could not confirm the values
    /// left the page: that keeps the session paused through every later
    /// fill, until one's cleanup confirms the page holds none.
    pub(super) fn fill_resume(
        &mut self,
        clear_after_ms: Option<u64>,
        audit: &mut Map<String, Value>,
    ) -> Result<Value, Failure> {
        if self.fill.unclean {
            return Err(Failure::new(
                code::session_paused,
                "a value latch filled may still be on the page; the session stays paused (close it to end it)".into(),
            ));
        }
        self.fill.targets.clear();
        self.paused = false;
        if let Some(ms) = clear_after_ms
            && self.holding().ok() != Some(0)
            && !self.fill.guarded.is_empty()
        {
            // An earlier secret still on the page keeps its deadline: the
            // earliest goes first.
            let at = Instant::now() + Duration::from_millis(ms);
            self.fill.clear_at = Some(self.fill.clear_at.map_or(at, |was| was.min(at)));
        }
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

    /// When the secrets a fill left on the page are due to be emptied; none
    /// while a fill holds the session, which keeps the deadline for after.
    pub fn clear_at(&self) -> Option<Instant> {
        self.fill.clear_at.filter(|_| !self.paused)
    }

    /// Empties the filled fields of the active tab once their time is up;
    /// a filled tab that isn't the active one is tried again a little
    /// later, as only the active tab's frames can be read.
    pub fn clear_due(&mut self) {
        if self.clear_at().is_none_or(|at| at > Instant::now()) {
            return;
        }
        let Ok(filled) = self.live_filled() else {
            self.fill.clear_at = Some(Instant::now() + CLEAR_AGAIN);
            return;
        };
        for (_, frame) in &filled.here {
            let _ = self.driver.run(frame, "__latch.clearFilled()");
        }
        self.fill.clear_at = if filled.elsewhere.is_empty() {
            // Lifts the GET guard once nothing is held.
            let _ = self.holding();
            None
        } else {
            Some(Instant::now() + CLEAR_AGAIN)
        };
    }

    /// `take` (a screenshot) with every field of the active tab that holds
    /// a value latch filled shown blank, and their own look put back after.
    /// Refused when a filled frame there can't be blanked, or a field's
    /// blanking was gone once the picture was taken.
    pub(super) fn blanked<T>(
        &mut self,
        take: impl FnOnce(&mut D) -> Result<T, Failure>,
    ) -> Result<T, Failure> {
        let here = self.live_filled()?.here;
        let mut done = Vec::new();
        let mut failed = false;
        for (_, frame) in &here {
            match self.driver.run(frame, "__latch.blankFilled(true)") {
                Ok(_) => done.push(frame.clone()),
                Err(_) => {
                    failed = true;
                    break;
                }
            }
        }
        let mut taken = if failed {
            Err(unhidden())
        } else {
            take(&mut self.driver)
        };
        if taken.is_ok()
            && done.iter().any(|frame| {
                self.driver.run(frame, "__latch.stillBlank()").ok() != Some(json!(true))
            })
        {
            taken = Err(unhidden());
        }
        for frame in &done {
            let _ = self.driver.run(frame, "__latch.blankFilled(false)");
        }
        taken
    }

    /// `eval` is refused while a filled value may still be on a page of
    /// the session, in any of its tabs.
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

/// A screenshot refused: a filled value would show in it.
fn unhidden() -> Failure {
    Failure::new(
        code::secret_on_page,
        "a value latch filled from the vault is on this page in a field latch could not hide; a screenshot waits until it is gone".into(),
    )
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
        let cleaned = core.call(Call::FillCleanup, None).err().unwrap();
        assert_eq!(cleaned.code, "engine_error");
        // As after a later fill that failed before releasing anything.
        let resumed = core
            .call(
                Call::FillResume {
                    clear_after_ms: None,
                },
                None,
            )
            .err()
            .unwrap();
        assert_eq!(resumed.code, "session_paused");
        assert!(core.paused);
    }
}

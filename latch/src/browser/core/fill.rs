//! The browser's half of a vault fill (`login`, `fill`, `fill-code`). The
//! host runs the steps in order, holding the session the whole time:
//!
//! 1. `PrepareFill` pauses the session (every agent call now gets
//!    `session_paused`, and the worker answers every other session on the
//!    browser the same), picks the fields from the snapshot unless the
//!    agent named them, and reports for each where a value would go: the
//!    field frame's effective origin (`self.origin` in latch's isolated
//!    world), the top-level origin, and the document's token;
//! 2. the host checks those origins against the item and releases values,
//!    one `SetValue` each: the driver re-checks element, document and
//!    origin and sets the value in one step, never by typing;
//! 3. `FillSubmit` clicks the submit (or `--press`) button; on the first
//!    page of a two-page login, a second `PrepareFill` waits for the
//!    password field, and steps 2 and 3 run again there;
//! 4. `FillCleanup` waits until each document a value went into is
//!    replaced or its filled fields are empty, clearing them itself when
//!    the page keeps them; if that cannot be confirmed the session stays
//!    paused;
//! 5. `FillResume` lets the agent back in, never while an unconfirmed
//!    cleanup, of this fill or an earlier one, stands.
//!
//! No step answers with a value. While a filled value is still on the page
//! (a fill without a submit), `eval` is refused.

use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::super::driver::{Driver, Frame};
use super::{Core, stale};
use crate::failure::{Exit, Failure};

/// How long a submitted form has to replace or empty its document.
const CLEANUP: Duration = Duration::from_secs(15);
/// How long latch watches fields it emptied before calling them empty.
const SETTLED: Duration = Duration::from_millis(500);
/// How long the second page of a two-page login has to show its password
/// field.
const PASSWORD_STEP: Duration = Duration::from_secs(15);

#[derive(Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Login,
    Code,
    Fields,
}

#[derive(Deserialize, Debug)]
pub struct Prepare {
    pub snapshot: Option<String>,
    pub mode: Mode,
    /// Refs the agent named: `username`, `password`, `code`, or an item
    /// field's label; several refs split one value across boxes.
    #[serde(default)]
    pub fields: Vec<(String, Vec<String>)>,
    pub submit: Option<String>,
    /// The second step of a login whose first took the username alone:
    /// wait for a password field, read the page afresh, and pick there.
    #[serde(default)]
    pub password_step: bool,
}

/// Item labels (or `username`, `password`, `code`) and the refs each goes into.
type Fields = Vec<(String, Vec<String>)>;
/// A snapshot's frame, with its refs and their indexes in that frame.
type FrameRefs = (Frame, Vec<(String, usize)>);
/// The button that ends a field's step, when latch can tell one, and every
/// button that could.
type Ends = (Option<String>, Vec<String>);

/// One field a value goes into.
struct Target {
    reference: String,
    frame: Frame,
    snapshot: String,
    index: usize,
    origin: String,
    token: String,
}

#[derive(Default)]
pub struct State {
    /// The snapshot the fill was prepared on.
    snapshot: Option<String>,
    targets: Vec<Target>,
    /// Frames a value went into, to check for leftovers and clean.
    filled: Vec<Frame>,
    /// A cleanup could not confirm the values left the page; until a later
    /// one does, nobody is let back in.
    unclean: bool,
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
        if prepare.password_step {
            self.password_shown()?;
        }
        self.page(audit)?;
        let snapshot = match prepare.snapshot.as_deref() {
            // latch's own reading of the password page, not the agent's.
            _ if prepare.password_step => {
                self.quiet = true;
                self.read(None)?.snapshot
            }
            Some(id) => self.snapshot(id)?.id.clone(),
            None => self.snapshots.back().ok_or_else(stale)?.id.clone(),
        };
        let (fields, submit) = match prepare.mode {
            // Named fields of a login or a code still go out through their
            // form's submit: found, or the fill stops before any release.
            Mode::Login | Mode::Code if !prepare.fields.is_empty() => {
                let submit = match prepare.submit {
                    Some(submit) => submit,
                    None => self.form_submit(&snapshot, &prepare.fields)?,
                };
                (prepare.fields, Some(submit))
            }
            _ if !prepare.fields.is_empty() => (prepare.fields, prepare.submit),
            Mode::Login => self.pick_login(&snapshot, prepare.submit)?,
            Mode::Code => self.pick_code(&snapshot, prepare.submit)?,
            Mode::Fields => {
                return Err(Failure::usage(
                    "bad_arguments",
                    "fill needs --field LABEL=eN for each field".into(),
                ));
            }
        };
        let main = self.main_frame()?;
        let top = self
            .driver
            .run(&main, "self.origin === 'null' ? 'opaque' : self.origin")?;
        let top = top.as_str().unwrap_or("opaque").to_string();
        let mut targets = Vec::new();
        let mut reported = Vec::new();
        for (key, refs) in &fields {
            let mut boxes = Vec::new();
            for reference in refs {
                let (id, position) = self.bound(Some(&snapshot), reference)?;
                let (frame, index) = self.reference(&id, position)?;
                let info = self
                    .driver
                    .run(&frame, &format!("__latch.fillTarget({id:?}, {index})"))?;
                if info["gone"] == true {
                    return Err(stale());
                }
                boxes.push(json!({
                    "ref": reference,
                    "origin": info["origin"],
                    "kind": info["kind"],
                    "max_length": info["max_length"],
                    "placeholder": info["placeholder"],
                    "page_secret": info["page_secret"],
                }));
                targets.push(Target {
                    reference: reference.clone(),
                    frame,
                    snapshot: id,
                    index,
                    origin: info["origin"].as_str().unwrap_or("opaque").to_string(),
                    token: info["token"].as_str().unwrap_or_default().to_string(),
                });
            }
            reported.push(json!({"field": key, "boxes": boxes}));
        }
        if let Some(submit) = &submit {
            self.bound(Some(&snapshot), submit)?;
        }
        self.fill.targets = targets;
        self.fill.snapshot = Some(snapshot.clone());
        Ok(json!({"snapshot": snapshot, "top_origin": top, "fields": reported, "submit": submit}))
    }

    /// Waits for the second page of a two-page login: a password field
    /// shown in some frame of the active tab.
    fn password_shown(&mut self) -> Result<(), Failure> {
        let deadline = Instant::now() + PASSWORD_STEP;
        loop {
            for frame in self.driver.frames()? {
                if self.driver.run(&frame, "__latch.passwordShown()").ok() == Some(json!(true)) {
                    return Ok(());
                }
            }
            if Instant::now() >= deadline || self.driver.dialog_open() {
                return Err(Failure::failed(
                    "timeout",
                    "latch filled the username, but no password field came after it; observe the page (it may want a code, a passkey or the owner)".into(),
                ));
            }
            std::thread::sleep(Duration::from_millis(200));
            self.driver.settle()?;
        }
    }

    /// The login form on the snapshot's page: one password field with the
    /// text or email field before it and its form's submit button; or, on
    /// the first page of a two-page login, one username field alone.
    fn pick_login(
        &mut self,
        snapshot: &str,
        submit: Option<String>,
    ) -> Result<(Fields, Option<String>), Failure> {
        let mut logins = Vec::new();
        let mut users = Vec::new();
        for (frame, refs) in self.frames_of(snapshot)? {
            let Some(found) =
                self.in_frame(&frame, &format!("__latch.loginFields({snapshot:?})"))?
            else {
                continue;
            };
            let to_ref = |index: &Value| {
                index
                    .as_i64()
                    .filter(|i| *i >= 0)
                    .and_then(|i| refs.iter().find(|(_, at)| *at == i as usize))
                    .map(|(r, _)| r.clone())
            };
            for login in found["logins"].as_array().into_iter().flatten() {
                logins.push((
                    to_ref(&login["username"]),
                    to_ref(&login["password"]),
                    ends(login, &to_ref),
                ));
            }
            for user in found["users"].as_array().into_iter().flatten() {
                users.push((to_ref(&user["username"]), None, ends(user, &to_ref)));
            }
        }
        let candidates = if logins.is_empty() { users } else { logins };
        let [(username, password, found_submit)] = candidates.as_slice() else {
            return Err(ambiguous(
                if candidates.is_empty() {
                    "no login field on this page; name them with --username/--password"
                } else {
                    "more than one login form on this page; name the fields with --username/--password"
                },
                candidates
                    .iter()
                    .flat_map(|(u, p, _)| [u.clone(), p.clone()])
                    .flatten()
                    .collect(),
                snapshot,
            ));
        };
        let mut fields = Vec::new();
        if let Some(username) = username {
            fields.push(("username".to_string(), vec![username.clone()]));
        }
        if let Some(password) = password {
            fields.push(("password".to_string(), vec![password.clone()]));
        }
        let submit = step_end(submit, found_submit.clone(), "the login form", snapshot)?;
        Ok((fields, Some(submit)))
    }

    /// The one one-time-code field on the page and its form's submit button.
    fn pick_code(
        &mut self,
        snapshot: &str,
        submit: Option<String>,
    ) -> Result<(Fields, Option<String>), Failure> {
        let mut codes = Vec::new();
        for (frame, refs) in self.frames_of(snapshot)? {
            let Some(found) =
                self.in_frame(&frame, &format!("__latch.codeFields({snapshot:?})"))?
            else {
                continue;
            };
            for code in found.as_array().into_iter().flatten() {
                let to_ref = |index: &Value| {
                    index
                        .as_i64()
                        .filter(|i| *i >= 0)
                        .and_then(|i| refs.iter().find(|(_, at)| *at == i as usize))
                        .map(|(r, _)| r.clone())
                };
                codes.push((to_ref(&code["code"]), ends(code, &to_ref)));
            }
        }
        let [(Some(code), found_submit)] = codes.as_slice() else {
            return Err(ambiguous(
                "latch cannot tell the one code field on this page; name it with --field",
                codes.iter().filter_map(|(c, _)| c.clone()).collect(),
                snapshot,
            ));
        };
        let submit = step_end(submit, found_submit.clone(), "the code's form", snapshot)?;
        Ok((vec![("code".into(), vec![code.clone()])], Some(submit)))
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

    /// The button that submits the form holding the first named field.
    fn form_submit(&mut self, snapshot: &str, fields: &Fields) -> Result<String, Failure> {
        let Some(reference) = fields.first().and_then(|(_, refs)| refs.first()) else {
            return step_end(None, (None, Vec::new()), "this form", snapshot);
        };
        let (id, position) = self.bound(Some(snapshot), reference)?;
        let (frame, index) = self.reference(&id, position)?;
        let found = self
            .driver
            .run(&frame, &format!("__latch.submitFor({id:?}, {index})"))?;
        let refs = self
            .frames_of(snapshot)?
            .into_iter()
            .find(|(f, _)| f.id == frame.id)
            .map(|(_, refs)| refs)
            .unwrap_or_default();
        let to_ref = |index: &Value| {
            index
                .as_i64()
                .filter(|i| *i >= 0)
                .and_then(|i| refs.iter().find(|(_, at)| *at == i as usize))
                .map(|(r, _)| r.clone())
        };
        step_end(None, ends(&found, &to_ref), "this form", snapshot)
    }

    /// Each frame of a snapshot, with its refs and their in-frame indexes.
    fn frames_of(&self, snapshot: &str) -> Result<Vec<FrameRefs>, Failure> {
        let mut frames: Vec<FrameRefs> = Vec::new();
        for (position, entry) in self.snapshot(snapshot)?.refs.iter().enumerate() {
            let reference = (format!("e{}", position + 1), entry.index);
            match frames.iter_mut().find(|(f, _)| f.id == entry.frame.id) {
                Some((_, refs)) => refs.push(reference),
                None => frames.push((entry.frame.clone(), vec![reference])),
            }
        }
        Ok(frames)
    }

    /// One value into one prepared field, after checking its element,
    /// document and origin again. Answers whether the field holds it.
    pub(super) fn set_value(&mut self, target: &str, value: &str) -> Result<Value, Failure> {
        let Some(at) = self.fill.targets.iter().position(|t| t.reference == target) else {
            return Err(Failure::usage(
                "bad_request",
                format!("{target} was not prepared for this fill"),
            ));
        };
        let frames = self.driver.frames()?;
        let target = &self.fill.targets[at];
        let refused = |why: &str| {
            Failure::new(
                Exit::Denied,
                "fill_refused",
                format!("nothing was filled: {why}"),
            )
        };
        if !frames
            .iter()
            .any(|f| f.id == target.frame.id && f.document == target.frame.document)
        {
            return Err(refused("the document was replaced"));
        }
        let (frame, snapshot, index, origin, token) = (
            target.frame.clone(),
            target.snapshot.clone(),
            target.index,
            target.origin.clone(),
            target.token.clone(),
        );
        let answer = self.driver.call(
            &frame,
            "function (s, i, v, o, t) { return __latch.fill(s, i, v, o, t); }",
            vec![
                json!(snapshot),
                json!(index),
                json!(value),
                json!(origin),
                json!(token),
            ],
        )?;
        if !self
            .fill
            .filled
            .iter()
            .any(|f| f.id == frame.id && f.document == frame.document)
        {
            self.fill.filled.push(frame);
        }
        if let Some(why) = answer["refused"].as_str() {
            return Err(refused(why));
        }
        Ok(json!({"held": answer["held"] == true}))
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
                return Err(Failure::failed(
                    "context_changed",
                    "the button, or the lines around it, changed after the fill was confirmed; it was not pressed".into(),
                ));
            }
        }
        let (snapshot, position) = self.bound(prepared.as_deref(), target)?;
        let at = self.locate(&snapshot, position)?;
        self.driver.click(at.x, at.y)?;
        let events = self.driver.settle()?;
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
        loop {
            if self.holding().ok() == Some(0) {
                self.fill.unclean = false;
                return Ok(json!({"clean": true, "cleared_by_latch": false}));
            }
            if Instant::now() >= deadline || self.driver.dialog_open() {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
            let _ = self.driver.settle();
        }
        for frame in self.live_filled()? {
            let _ = self.driver.run(&frame, "__latch.clearFilled()");
        }
        // Clean only on a positive answer, given after the page had a moment
        // to put a value back: a frame that cannot be read may still hold it.
        std::thread::sleep(SETTLED);
        let clean = self.holding().ok() == Some(0);
        self.fill.unclean = !clean;
        Ok(json!({"clean": clean, "cleared_by_latch": clean}))
    }

    /// Lets the agent back in, unless a cleanup could not confirm the values
    /// left the page: that keeps the session paused through every later
    /// fill, until one's cleanup confirms the page holds none.
    pub(super) fn fill_resume(&mut self, audit: &mut Map<String, Value>) -> Result<Value, Failure> {
        if self.fill.unclean {
            return Err(Failure::failed(
                "session_paused",
                "a value latch filled may still be on the page; the session stays paused (close it to end it)".into(),
            ));
        }
        self.fill.targets.clear();
        self.paused = false;
        let before = Map::new();
        let mut result = Map::new();
        let events = self.driver.settle()?;
        self.after(&before, events, &mut result)?;
        result.extend(self.page(audit)?);
        Ok(Value::Object(result))
    }

    /// Filled frames whose document is still the one filled.
    fn live_filled(&mut self) -> Result<Vec<Frame>, Failure> {
        let frames = self.driver.frames()?;
        self.fill.filled.retain(|f| {
            frames
                .iter()
                .any(|now| now.id == f.id && now.document == f.document)
        });
        Ok(self.fill.filled.clone())
    }

    /// How many filled fields still hold a value; an error when a filled
    /// document cannot be read.
    fn holding(&mut self) -> Result<u64, Failure> {
        let mut total = 0;
        for frame in self.live_filled()? {
            // A frame whose document is still the filled one but cannot be
            // read is not known to be empty.
            let count = self.driver.run(&frame, "__latch.holding()")?;
            total += count.as_u64().ok_or_else(|| {
                Failure::failed(
                    "engine_error",
                    "latch could not count the filled fields".into(),
                )
            })?;
        }
        Ok(total)
    }

    /// `eval` is refused while a filled value is still on the page.
    pub(super) fn no_value_on_page(&mut self) -> Result<(), Failure> {
        if self.holding().ok() != Some(0) {
            return Err(Failure::new(
                Exit::Denied,
                "secret_on_page",
                "a value latch filled from the vault is still on this page; eval waits until it is gone".into(),
            ));
        }
        Ok(())
    }
}

/// The reader's answer for the buttons that could end a field's step, as
/// refs: the one latch can tell, and every candidate.
fn ends(found: &Value, to_ref: &impl Fn(&Value) -> Option<String>) -> Ends {
    (
        to_ref(&found["submit"]),
        found["submits"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(to_ref)
            .collect(),
    )
}

/// The button that ends a step: the one the agent named, else the one latch
/// can tell. With several that could, or none, the agent names it.
fn step_end(
    named: Option<String>,
    ends: Ends,
    form: &str,
    snapshot: &str,
) -> Result<String, Failure> {
    let (found, candidates) = ends;
    if let Some(submit) = named.or(found) {
        return Ok(submit);
    }
    let message = if candidates.is_empty() {
        format!("{form} has no submit button latch can tell; name it with --submit")
    } else {
        format!("more than one button could submit {form}; name it with --submit")
    };
    Err(ambiguous(&message, candidates, snapshot))
}

/// The candidates are refs in `snapshot`: on a login's second page, a
/// reading latch took itself.
fn ambiguous(message: &str, candidates: Vec<String>, snapshot: &str) -> Failure {
    let mut failure = Failure::failed("ambiguous_target", message.to_string());
    failure.details = json!({"candidates": candidates, "snapshot": snapshot});
    failure
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
            Err(Failure::failed(
                "engine_error",
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
        fn back_url(&mut self) -> Result<Option<String>, Failure> {
            unreachable!()
        }
        fn run(&mut self, _: &Frame, _: &str) -> Result<Value, Failure> {
            unreachable!()
        }
        fn call(&mut self, _: &Frame, _: &str, _: Vec<Value>) -> Result<Value, Failure> {
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
        core.fill.filled.push(Frame {
            id: "f1".into(),
            parent: None,
            document: "d1".into(),
        });
        let cleaned = core.call(Call::FillCleanup, None).err().unwrap();
        assert_eq!(cleaned.code, "engine_error");
        // As after a later fill that failed before releasing anything.
        let resumed = core.call(Call::FillResume, None).err().unwrap();
        assert_eq!(resumed.code, "session_paused");
        assert!(core.paused);
    }
}

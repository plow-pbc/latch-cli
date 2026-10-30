//! The shared core: one browser session's agent commands, written once for
//! every engine on top of the driver contract (`driver.rs`).
//!
//! It owns snapshots and refs. A snapshot (`o3`) is one reading of the
//! active tab by latch's page reader (`reader.js`), every frame included;
//! a ref (`e7`) is one element in it, bound to its frame's document. A ref
//! whose document was replaced, or whose element is gone, is refused with
//! `stale_snapshot` and never guessed again.
//!
//! Each answer carries the result for the agent and, separately, what the
//! host's audit may record: never page text, field values or typed text.

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::driver::{Driver, Event, Frame};

mod fill;
mod look;
use crate::failure::{Failure, code};
pub use look::{Condition, fingerprint};

/// The most elements one snapshot lists, across all frames.
pub const MAX_ELEMENTS: usize = 200;
/// Snapshots kept per session; refs in older ones are stale. A plan's
/// steps may each name a ref from a page latch returned earlier, so this
/// keeps several.
const KEEP: usize = 16;
/// Page text: `observe`'s preview and `text`'s whole answer.
const PREVIEW_CHARS: usize = 1500;
const TEXT_CHARS: usize = 20_000;
/// The largest JSON result an `eval` may return.
const MAX_EVAL_RESULT: usize = 1024 * 1024;
/// How far one `scroll` moves, in CSS pixels.
const SCROLL: f64 = 600.0;
/// How long one call may take, every wait for the engine in it together;
/// typing adds time per character, and a wait its own.
pub const CALL: Duration = Duration::from_secs(60);
const PER_CHARACTER: Duration = Duration::from_millis(200);
/// The most frames one snapshot reads; the rest are left out (`truncated`).
pub const MAX_FRAMES: usize = 50;

/// One call from the host, as the worker reads it.
#[derive(Deserialize, Debug)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Call {
    Open {
        url: Option<String>,
    },
    Observe {
        tab: Option<String>,
        scope: Option<String>,
    },
    Text,
    Screenshot,
    /// A screenshot for a plan's trail.
    Shot,
    Act {
        snapshot: Option<String>,
        action: Action,
        target: Option<String>,
        text: Option<String>,
        /// A step of a plan: the answer carries no element list, and the
        /// latest snapshot shown the agent stays what it was.
        #[serde(default)]
        quiet: bool,
        /// What the host judged: the action is made only if what it acts
        /// on, and the lines around it, still hash to this.
        #[serde(default)]
        guard: Option<Guard>,
    },
    /// The elements with `role`, read afresh, and those whose accessible
    /// name is exactly `name`: `find`. The agent sees only the candidates.
    Resolve {
        role: String,
        name: String,
    },
    /// The snapshot a ref is read in (`snapshot`, else the latest shown),
    /// checked to hold it: how a plan pins its refs before it runs.
    Pin {
        snapshot: Option<String>,
        target: String,
    },
    /// Each target's role and name as its snapshot (`snapshot`, else the
    /// latest shown) listed them, read from nothing on the page now: what
    /// the judge is shown beside a plan step's ref. `null` for one it
    /// doesn't hold.
    Named {
        targets: Vec<Named>,
    },
    /// What a click (or, with `enter`, an Enter in a field) would act on,
    /// and the lines around it: what the host judges the action's
    /// consequence on, and binds a confirmation to.
    Review {
        snapshot: Option<String>,
        target: String,
        #[serde(default)]
        enter: bool,
    },
    /// The page now: where it is, its first lines and an element list.
    Glance,
    /// Where the active tab is: its URL and title, nothing read from it.
    Page,
    /// The page's lines and its non-secret field values, for a judgement.
    Lines,
    /// The page as one pass of an instruction step sees it; with `step`,
    /// which lines on screen were written since that step began (`begin`
    /// starts it here).
    View {
        #[serde(default)]
        step: Option<String>,
        #[serde(default)]
        begin: bool,
    },
    /// Waits up to `within_ms` for `condition`.
    Wait {
        condition: Condition,
        within_ms: u64,
    },
    Tab {
        action: TabAction,
        tab: Option<String>,
        url: Option<String>,
    },
    Dialog {
        accept: bool,
        text: Option<String>,
    },
    Tabs,
    /// A handoff begins: the session's window to the front for the owner.
    Front,
    /// A handoff has ended: every ref from before it is refused.
    Forget,
    /// The agent's script; the host audits it.
    Eval {
        script: String,
    },
    /// `size` bytes of the file follow the call's line.
    Upload {
        snapshot: Option<String>,
        target: String,
        name: String,
        size: u64,
    },
    Downloads,
    DownloadGet {
        download: String,
    },
    /// Every element of a snapshot as a fill sees it; pauses nothing.
    FillInventory {
        #[serde(default)]
        snapshot: Option<String>,
    },
    /// The steps of a vault fill, which the host runs in order (`fill.rs`).
    PrepareFill(fill::Prepare),
    SetValue {
        target: String,
        value: String,
        /// A secret (a password, a code, a card or identity secret): no GET
        /// request may carry it while it may be on the page.
        #[serde(default)]
        secret: bool,
        /// A value sites format as they like (a card's number or expiry): the field
        /// keeps it whatever separators either side puts in it.
        #[serde(default)]
        formatted: bool,
    },
    FillSubmit {
        target: String,
        /// A confirmed fill's press: made only while the button and the
        /// lines around it are as the agent confirmed them.
        #[serde(default)]
        guard: Option<Guard>,
    },
    /// After a login's identifier stage: its next stage, read privately.
    NextStage,
    FillCleanup,
    /// With `clear_after_ms`, a secret the fill left on the page is emptied
    /// that long after, if its field still holds it (`Core::clear_due`).
    FillResume {
        #[serde(default)]
        clear_after_ms: Option<u64>,
    },
    /// Closes the browser, ending the session.
    Close,
}

impl Call {
    /// How long this call may take, from its line to its answer, when a
    /// call with nothing to type or wait for has `base` (`CALL`).
    pub fn limit(&self, base: Duration) -> Duration {
        match self {
            Call::Act {
                text: Some(text), ..
            } => base + PER_CHARACTER * text.chars().count() as u32,
            Call::Wait { within_ms, .. } => base + Duration::from_millis(*within_ms),
            _ => base,
        }
    }

    /// The address this call loads, when it names one.
    pub fn loads(&self) -> Option<&str> {
        match self {
            Call::Open { url: Some(url) }
            | Call::Tab {
                action: TabAction::New,
                url: Some(url),
                ..
            } => Some(url),
            _ => None,
        }
    }
}

/// The reviewed context an action was judged on (`Call::Review`, with
/// `enter`), as its [`fingerprint`].
#[derive(Deserialize, Debug)]
pub struct Guard {
    pub fingerprint: String,
    #[serde(default)]
    pub enter: bool,
}

#[derive(Deserialize, Serialize, Debug, Clone, Copy, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Click,
    Type,
    Select,
    Press,
    Scroll,
    Back,
}

#[derive(Deserialize, Debug, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum TabAction {
    New,
    Switch,
    Close,
}

struct Ref {
    frame: Frame,
    index: usize,
    /// Its ref's number: `e7` is 7.
    number: usize,
    /// Its role and accessible name, as the reading listed them: text the
    /// page wrote.
    role: String,
    name: String,
}

struct Snapshot {
    id: String,
    tab: String,
    refs: Vec<Ref>,
}

/// A ref in the snapshot it is read in (else the latest shown), for
/// `Call::Named`.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Named {
    #[serde(default)]
    pub snapshot: Option<String>,
    pub target: String,
}

/// A fresh reading of the active tab, as the agent sees it.
struct Reading {
    snapshot: String,
    elements: Vec<Value>,
    frames: Vec<Value>,
    truncated: bool,
}

impl Reading {
    fn into_json(self, into: &mut Map<String, Value>) {
        into.insert("snapshot".into(), json!(self.snapshot));
        into.insert("elements".into(), json!(self.elements));
        if self.frames.len() > 1 {
            into.insert("frames".into(), json!(self.frames));
        }
        if self.truncated {
            into.insert("truncated".into(), json!(true));
        }
    }
}

/// A call's answer: what the agent gets, and what the audit records.
pub struct Answer {
    pub result: Value,
    pub audit: Value,
    /// A screenshot's bytes, sent after the answer line.
    pub bytes: Option<Vec<u8>>,
}

pub struct Core<D: Driver> {
    pub driver: D,
    snapshots: VecDeque<Snapshot>,
    next_snapshot: u32,
    /// Bytes the current call sends after its answer: a screenshot, a download.
    bytes: Option<Vec<u8>>,
    uploads: u32,
    /// The agent is shut out: a fill is under way, or its cleanup could not
    /// be confirmed.
    paused: bool,
    fill: fill::State,
    /// The current call's readings are latch's own, not shown the agent.
    quiet: bool,
    /// The latest snapshot shown the agent: what a ref without
    /// `--snapshot` is read in.
    last_shown: Option<String>,
    /// Each element's ref number, by its tab, and its frame, document and
    /// identity there: an element keeps its ref from one reading to the
    /// next, and a new one gets the next number, never one another element
    /// had.
    numbers: HashMap<String, HashMap<(String, String, u64), usize>>,
    next_number: usize,
    /// When the current call's reading stops starting frames.
    read_until: Option<Instant>,
    /// How long the current call has waited for pages to settle (and a
    /// `wait` for its condition), if it has: its audit's `settle_ms`.
    settled: Option<Duration>,
}

/// Where a ref's element is, ready for input.
struct Located {
    x: f64,
    y: f64,
    kind: String,
    options: Vec<String>,
}

impl<D: Driver> Core<D> {
    pub fn new(driver: D) -> Self {
        Core {
            driver,
            snapshots: VecDeque::new(),
            next_snapshot: 0,
            bytes: None,
            uploads: 0,
            paused: false,
            fill: fill::State::default(),
            quiet: false,
            last_shown: None,
            numbers: HashMap::new(),
            next_number: 0,
            read_until: None,
            settled: None,
        }
    }

    /// The driver's `settle`, its time counted in the call's `settle_ms`.
    fn settle(&mut self) -> Result<Vec<Event>, Failure> {
        let settling = Instant::now();
        let events = self.driver.settle();
        self.settled = Some(self.settled.unwrap_or_default() + settling.elapsed());
        events
    }

    /// Bounds the call about to run (`Driver::limit`). A reading stops
    /// starting frames with a quarter of the call's time left, for the
    /// rest of the call.
    pub fn limit(&mut self, deadline: Instant) {
        let left = deadline.saturating_duration_since(Instant::now());
        self.read_until = Some(deadline - left / 4);
        self.driver.limit(Some(deadline));
    }

    /// Runs one call. Close is the worker's to do, not the core's.
    /// The audit record goes with an error too, as its details' `audit`,
    /// which the host takes out before the agent sees the error.
    /// `input` is an upload's bytes.
    pub fn call(&mut self, call: Call, input: Option<Vec<u8>>) -> Result<Answer, Failure> {
        let mut audit = Map::new();
        self.settled = None;
        let fill_step = matches!(
            call,
            Call::PrepareFill(_)
                | Call::SetValue { .. }
                | Call::FillSubmit { .. }
                | Call::NextStage
                | Call::FillCleanup
                | Call::FillResume { .. }
                | Call::Tabs
        );
        if self.paused && !fill_step {
            let mut failure = Failure::new(
                code::session_paused,
                "a vault fill holds this session; nothing can be read or done until it finishes"
                    .into(),
            );
            failure.details = json!({"audit": {}});
            return Err(failure);
        }
        self.quiet = matches!(
            call,
            Call::Act { quiet: true, .. }
                | Call::Resolve { .. }
                | Call::Lines
                | Call::View { .. }
                | Call::Shot
                | Call::Wait { .. }
                | Call::Pin { .. }
                | Call::Named { .. }
                | Call::Review { .. }
                | Call::FillInventory { .. }
        );
        let result = match call {
            Call::Resolve { role, name } => self.resolve(&role, &name, &mut audit),
            Call::Pin { snapshot, target } => self.pin(snapshot.as_deref(), &target),
            Call::Named { targets } => Ok(self.named(&targets)),
            Call::Review {
                snapshot,
                target,
                enter,
            } => self.review(snapshot.as_deref(), &target, enter, &mut audit),
            Call::Glance => self.glance(&mut audit),
            Call::Page => self.page(&mut audit).map(Value::Object),
            Call::Lines => self.lines(&mut audit),
            Call::View { step, begin } => self.view(step.as_deref(), begin, &mut audit),
            Call::Wait {
                condition,
                within_ms,
            } => {
                let waiting = Instant::now();
                let waited = self.wait(condition, within_ms, &mut audit);
                // All of it is waiting, its own settling included.
                self.settled = Some(waiting.elapsed());
                waited
            }
            Call::FillInventory { snapshot } => self.fill_inventory(snapshot.as_deref()),
            Call::PrepareFill(prepare) => self.prepare_fill(prepare, &mut audit),
            Call::NextStage => self.next_stage(),
            Call::SetValue {
                target,
                value,
                secret,
                formatted,
            } => self.set_value(&target, &value, secret, formatted),
            Call::FillSubmit { target, guard } => self.fill_submit(&target, guard.as_ref()),
            Call::FillCleanup => self.fill_cleanup(),
            Call::FillResume { clear_after_ms } => self.fill_resume(clear_after_ms, &mut audit),
            Call::Open { url } => self.open(url.as_deref(), &mut audit),
            Call::Observe { tab, scope } => {
                self.observe(tab.as_deref(), scope.as_deref(), &mut audit)
            }
            Call::Text => self.text(&mut audit),
            Call::Screenshot | Call::Shot => self.screenshot(&mut audit),
            Call::Act {
                snapshot,
                action,
                target,
                text,
                quiet: _,
                guard,
            } => self.act(
                snapshot.as_deref(),
                action,
                target.as_deref(),
                text.as_deref(),
                guard.as_ref(),
                &mut audit,
            ),
            Call::Tab { action, tab, url } => {
                self.tab(action, tab.as_deref(), url.as_deref(), &mut audit)
            }
            Call::Dialog { accept, text } => self.dialog(accept, text.as_deref(), &mut audit),
            Call::Tabs => self.driver.tabs().map(|tabs| json!({"tabs": tabs})),
            Call::Front => self.driver.front().map(|()| json!({"front": true})),
            Call::Forget => {
                self.snapshots.clear();
                self.last_shown = None;
                Ok(json!({"forgot": true}))
            }
            Call::Eval { script } => self.eval(&script, &mut audit),
            Call::Upload {
                snapshot,
                target,
                name,
                size: _,
            } => self.upload(
                snapshot.as_deref(),
                &target,
                &name,
                input.unwrap_or_default(),
                &mut audit,
            ),
            Call::Downloads => self.downloads(&mut audit),
            Call::DownloadGet { download } => self.download_get(&download, &mut audit),
            Call::Close => unreachable!("the worker closes the browser"),
        };
        if let Some(settled) = self.settled {
            audit.insert("settle_ms".into(), json!(settled.as_millis() as u64));
        }
        match result {
            Ok(result) => Ok(Answer {
                bytes: self.bytes.take(),
                result,
                audit: Value::Object(audit),
            }),
            Err(mut failure) => {
                let mut details = failure.details.as_object().cloned().unwrap_or_default();
                details.insert("audit".into(), Value::Object(audit));
                failure.details = Value::Object(details);
                Err(failure)
            }
        }
    }

    /// The browser, for what the worker does between calls.
    pub fn browser(&mut self) -> &mut D {
        &mut self.driver
    }

    /// A fresh reading for an error's details, when the page can be read.
    pub fn reading_for(&mut self, failure: &mut Failure) {
        // During a fill the page may hold a vault value: no reading goes out.
        // A step of a plan gets its reading from the host, once the plan ends.
        if self.paused
            || self.quiet
            || !matches!(
                failure.code.as_str(),
                "stale_snapshot" | "covered_target" | "no_such_target"
            )
            || self.driver.dialog_open()
        {
            return;
        }
        if let Ok(reading) = self.read(None) {
            let mut details = failure.details.as_object().cloned().unwrap_or_default();
            reading.into_json(&mut details);
            failure.details = Value::Object(details);
        }
    }

    fn open(
        &mut self,
        url: Option<&str>,
        audit: &mut Map<String, Value>,
    ) -> Result<Value, Failure> {
        if let Some(url) = url {
            self.no_dialog()?;
            self.driver.navigate(url)?;
        }
        let mut result = self.page(audit)?;
        let reading = self.read(None)?;
        reading.into_json(&mut result);
        Ok(Value::Object(result))
    }

    fn observe(
        &mut self,
        tab: Option<&str>,
        scope: Option<&str>,
        audit: &mut Map<String, Value>,
    ) -> Result<Value, Failure> {
        if let Some(tab) = tab {
            self.driver.switch_tab(tab)?;
        }
        self.no_dialog()?;
        let scope = match scope {
            Some(target) => Some(self.bound(None, target)?),
            None => None,
        };
        let mut result = self.page(audit)?;
        let reading = self.read(scope)?;
        reading.into_json(&mut result);
        let main = self.main_frame()?;
        let text = self
            .driver
            .run(&main, &format!("__latch.text({PREVIEW_CHARS})"))?;
        result.insert("text".into(), text["text"].clone());
        result.insert("tabs".into(), json!(self.driver.tabs()?));
        let events = self.settle()?;
        if !events.is_empty() {
            result.insert("events".into(), json!(events));
        }
        Ok(Value::Object(result))
    }

    /// Refuses, before anything is dispatched, a command whose optional
    /// feature this engine lacks. The audit records which one.
    fn needs(&mut self, capability: &str, audit: &mut Map<String, Value>) -> Result<(), Failure> {
        if self
            .driver
            .capabilities()
            .iter()
            .any(|have| have == capability)
        {
            return Ok(());
        }
        audit.insert("capability".into(), json!(capability));
        let mut failure = Failure::new(
            code::unsupported_capability,
            format!("this session's engine has no {capability}"),
        );
        failure.details = json!({"capability": capability});
        Err(failure)
    }

    fn eval(&mut self, script: &str, audit: &mut Map<String, Value>) -> Result<Value, Failure> {
        self.page(audit)?;
        self.needs("eval", audit)?;
        self.no_dialog()?;
        self.no_value_on_page("eval")?;
        let value = self.driver.eval(script)?;
        let size = serde_json::to_vec(&value).map_or(0, |bytes| bytes.len());
        if size > MAX_EVAL_RESULT {
            return Err(Failure::new(
                code::output_too_large,
                format!("the script's result is {size} bytes; the limit is {MAX_EVAL_RESULT}"),
            ));
        }
        let events = self.settle()?;
        let mut result = Map::new();
        result.insert("value".into(), value);
        if !events.is_empty() {
            result.insert("events".into(), json!(events));
        }
        Ok(Value::Object(result))
    }

    fn upload(
        &mut self,
        snapshot: Option<&str>,
        target: &str,
        name: &str,
        bytes: Vec<u8>,
        audit: &mut Map<String, Value>,
    ) -> Result<Value, Failure> {
        audit.insert("target".into(), json!(target));
        let before = self.page(audit)?;
        self.needs("upload", audit)?;
        self.no_dialog()?;
        let (snapshot, position) = self.bound(snapshot, target)?;
        audit.insert("snapshot".into(), json!(snapshot));
        let (frame, index) = self.reference(&snapshot, position)?;
        let is_file = self.driver.run(
            &frame,
            &format!("__latch.isFileInput({snapshot:?}, {index})"),
        )?;
        if is_file.is_null() {
            return Err(stale());
        }
        if is_file != true {
            return Err(Failure::new(
                code::wrong_target,
                "upload needs a file input".into(),
            ));
        }
        // Each upload in its own folder, under the name the client gave.
        let name = Path::new(name)
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|n| !n.is_empty())
            .ok_or_else(|| Failure::new(code::bad_arguments, "the file needs a name".into()))?
            .to_string();
        self.uploads += 1;
        let folder = self.driver.upload_folder().join(self.uploads.to_string());
        fs::create_dir_all(&folder)
            .and_then(|()| fs::write(folder.join(&name), &bytes))
            .map_err(|err| Failure::new(code::upload_failed, err.to_string()))?;
        self.driver.set_files(
            &frame,
            &format!("__latch.elementAt({snapshot:?}, {index})"),
            &[folder.join(&name)],
        )?;
        let events = self.settle()?;
        let mut result = Map::new();
        result.insert("action".into(), json!("upload"));
        result.insert("target".into(), json!({"ref": target}));
        result.insert("file".into(), json!({"name": name, "size": bytes.len()}));
        result.insert("dispatched".into(), json!(true));
        self.after(&before, events, &mut result)?;
        Ok(Value::Object(result))
    }

    fn downloads(&mut self, audit: &mut Map<String, Value>) -> Result<Value, Failure> {
        self.page(audit)?;
        self.needs("downloads", audit)?;
        Ok(json!({"downloads": self.driver.downloads()?}))
    }

    /// A completed download's bytes, sent after the answer.
    fn download_get(&mut self, id: &str, audit: &mut Map<String, Value>) -> Result<Value, Failure> {
        self.page(audit)?;
        self.needs("downloads", audit)?;
        audit.insert("download".into(), json!(id));
        let (download, path) = self.driver.download_file(id)?;
        let (origin, path_only) = origin_and_path(&download.url);
        audit.insert("download_origin".into(), json!(origin));
        audit.insert("download_path".into(), json!(path_only));
        let bytes =
            fs::read(&path).map_err(|err| Failure::new(code::download_failed, err.to_string()))?;
        let result =
            json!({"download": download.download, "name": download.name, "size": bytes.len()});
        self.bytes = Some(bytes);
        Ok(result)
    }

    fn text(&mut self, audit: &mut Map<String, Value>) -> Result<Value, Failure> {
        self.no_dialog()?;
        let mut result = self.page(audit)?;
        let main = self.main_frame()?;
        let text = self
            .driver
            .run(&main, &format!("__latch.text({TEXT_CHARS})"))?;
        result.insert("text".into(), text["text"].clone());
        if text["truncated"] == true {
            result.insert("truncated".into(), json!(true));
        }
        Ok(Value::Object(result))
    }

    /// The fields holding a value latch filled show blank in the picture.
    fn screenshot(&mut self, audit: &mut Map<String, Value>) -> Result<Value, Failure> {
        self.no_dialog()?;
        self.page(audit)?;
        let (meta, png) = self.blanked(|driver| driver.screenshot())?;
        self.bytes = Some(png);
        Ok(meta)
    }

    fn act(
        &mut self,
        snapshot: Option<&str>,
        action: Action,
        target: Option<&str>,
        text: Option<&str>,
        guard: Option<&Guard>,
        audit: &mut Map<String, Value>,
    ) -> Result<Value, Failure> {
        audit.insert("action".into(), json!(action));
        self.no_dialog()?;
        let before = self.page(audit)?;
        let mut result = Map::new();
        result.insert("action".into(), json!(action));
        result.insert(
            "page".into(),
            json!({"url": before["url"], "title": before["title"]}),
        );
        match (action, target) {
            (Action::Back, _) => {
                if !self.driver.back()? {
                    return Err(Failure::new(
                        code::no_history,
                        "this tab has nothing to go back to".into(),
                    ));
                }
            }
            (Action::Scroll, None | Some("page")) => {
                let main = self.main_frame()?;
                let size = self.driver.run(&main, "[innerWidth, innerHeight]")?;
                let (x, y) = (
                    size[0].as_f64().unwrap_or(0.0) / 2.0,
                    size[1].as_f64().unwrap_or(0.0) / 2.0,
                );
                self.driver.wheel(x, y, scroll_delta(text)?)?;
                result.insert("target".into(), json!("page"));
            }
            (_, Some(target)) => {
                audit.insert("target".into(), json!(target));
                if let Some(snapshot) = snapshot {
                    audit.insert("snapshot".into(), json!(snapshot));
                }
                let (snapshot, position) = self.bound(snapshot, target)?;
                let element = self.element(&snapshot, position)?;
                audit.insert("snapshot".into(), json!(snapshot));
                audit.insert("role".into(), element["role"].clone());
                result.insert(
                    "target".into(),
                    json!({"ref": target, "role": element["role"], "name": element["name"], "row": element["row"]}),
                );
                if let Some(guard) = guard {
                    let now = self.review(Some(&snapshot), target, guard.enter, &mut Map::new())?;
                    if fingerprint(&now) != guard.fingerprint {
                        return Err(Failure::new(
                            code::context_changed,
                            "what this action acts on, or the lines around it, changed after latch judged it; nothing was sent".into(),
                        ));
                    }
                }
                self.dispatch(action, &snapshot, position, text)?;
            }
            _ => {
                return Err(Failure::new(
                    code::bad_target,
                    "act takes a ref (eN) from a page latch returned".into(),
                ));
            }
        }
        result.insert("dispatched".into(), json!(true));
        let events = self.settle()?;
        self.no_secret_sent()?;
        self.after(&before, events, &mut result)?;
        Ok(Value::Object(result))
    }

    /// What changed, the page's first lines, events, and a fresh element
    /// list, unless a dialog now blocks the page.
    fn after(
        &mut self,
        before: &Map<String, Value>,
        events: Vec<Event>,
        result: &mut Map<String, Value>,
    ) -> Result<(), Failure> {
        if !events.is_empty() {
            result.insert("events".into(), json!(events));
        }
        if self.driver.dialog_open() {
            result.insert("blocked_by_dialog".into(), json!(true));
            return Ok(());
        }
        let mut now = Map::new();
        let page = self.page(&mut now)?;
        let mut changed = Map::new();
        for key in ["tab", "url", "title"] {
            if page.get(key) != before.get(key) {
                changed.insert(key.into(), page[key].clone());
            }
        }
        if !changed.is_empty() {
            result.insert("changed".into(), Value::Object(changed));
        }
        if self.quiet {
            return Ok(());
        }
        result.insert("lines".into(), json!(self.first_lines(5)?));
        self.read(None)?.into_json(result);
        Ok(())
    }

    /// One input on the element at `position` in `snapshot`.
    fn dispatch(
        &mut self,
        action: Action,
        snapshot: &str,
        position: usize,
        text: Option<&str>,
    ) -> Result<(), Failure> {
        let (frame, index) = self.reference(snapshot, position)?;
        match action {
            Action::Click => {
                let at = self.locate(snapshot, position)?;
                self.driver.click(at.x, at.y)
            }
            Action::Type => {
                let text = required(text, "type needs the text to type")?;
                let at = self.locate(snapshot, position)?;
                if at.kind != "text" {
                    return Err(Failure::new(
                        code::wrong_target,
                        "type needs a text field".into(),
                    ));
                }
                self.driver.click(at.x, at.y)?;
                // A dialog's focus lock left active under a dialog stacked on
                // it puts the focus straight back where it was, and keys would
                // go there: the value goes in as a page's own script sets one,
                // with its input and change events.
                if !self.takes_focus(&frame, snapshot, index)? {
                    let held = self.driver.run(
                        &frame,
                        &format!("__latch.setText({snapshot:?}, {index}, {})", json!(text)),
                    )?;
                    if held != json!(text) {
                        return Err(Failure::new(
                            code::type_failed,
                            "the field would not take the focus, nor keep the value set in it"
                                .into(),
                        ));
                    }
                    return Ok(());
                }
                let typed = format!("__latch.typedInto({snapshot:?}, {index})");
                let before = self.driver.run(&frame, &typed)?;
                self.driver.select_all()?;
                self.driver.type_text(text)?;
                // A dialog the keys opened, or a page they replaced (an
                // Enter that sent the form), shows they were taken.
                if self.driver.dialog_open() {
                    return Ok(());
                }
                let after = match self.driver.run(&frame, &typed) {
                    Err(failure) if failure.code == "stale_snapshot" => return Ok(()),
                    after => after?,
                };
                // Keys the page moved on elsewhere were taken too.
                if after["focused"] == true
                    && after["value"] == before["value"]
                    && after["value"] != json!(text)
                {
                    return Err(Failure::new(
                        code::type_failed,
                        "the typing left the field as it was: the page did not take the keys"
                            .into(),
                    ));
                }
                Ok(())
            }
            Action::Select => {
                let choice = required(text, "select needs the option to choose")?;
                let at = self.locate(snapshot, position)?;
                if at.kind != "select" {
                    return Err(Failure::new(
                        code::wrong_target,
                        "select needs a drop-down (select)".into(),
                    ));
                }
                if !at.options.iter().any(|option| option == choice) {
                    let mut failure = Failure::new(
                        code::no_such_target,
                        format!("no option {choice:?} in that list"),
                    );
                    failure.details = json!({"options": at.options});
                    return Err(failure);
                }
                // Focused, the list takes the option's label typed as keys,
                // as a person picks one without opening it.
                self.focus(&frame, snapshot, index)?;
                self.driver.type_text(choice)?;
                let selected = self
                    .driver
                    .run(&frame, &format!("__latch.selected({snapshot:?}, {index})"))?;
                if selected != json!([choice]) {
                    return Err(Failure::new(
                        code::select_failed,
                        format!("typing {choice:?} chose {selected}; pick it another way"),
                    ));
                }
                Ok(())
            }
            Action::Press => {
                let key = required(text, "press needs a key, such as Enter")?;
                self.focus(&frame, snapshot, index)?;
                self.driver.press(key)
            }
            Action::Scroll => {
                let at = self.locate(snapshot, position)?;
                self.driver.wheel(at.x, at.y, scroll_delta(text)?)
            }
            Action::Back => unreachable!("handled without a target"),
        }
    }

    /// Gives the element the focus, so keys go to it; refused when it does
    /// not take it (hidden, disabled, or not focusable).
    fn focus(&mut self, frame: &Frame, snapshot: &str, index: usize) -> Result<(), Failure> {
        if !self.takes_focus(frame, snapshot, index)? {
            return Err(Failure::new(
                code::not_focused,
                "that element did not take the focus (hidden, disabled, not focusable, or a dialog keeps the focus to itself); no key was sent"
                    .into(),
            ));
        }
        Ok(())
    }

    /// Whether the element has the focus, given it if it has not.
    fn takes_focus(
        &mut self,
        frame: &Frame,
        snapshot: &str,
        index: usize,
    ) -> Result<bool, Failure> {
        let focused = self
            .driver
            .run(frame, &format!("__latch.focus({snapshot:?}, {index})"))?;
        if focused.is_null() {
            return Err(stale());
        }
        Ok(focused == true)
    }

    /// Brings the element into view and finds a point that reaches it,
    /// twice, so a target still moving is refused.
    fn locate(&mut self, snapshot: &str, position: usize) -> Result<Located, Failure> {
        let (frame, index) = self.reference(snapshot, position)?;
        let expression = format!("__latch.locate({snapshot:?}, {index})");
        let first = self.driver.run(&frame, &expression)?;
        std::thread::sleep(std::time::Duration::from_millis(80));
        let second = self.driver.run(&frame, &expression)?;
        let refused = |reason: &str| {
            let mut failure = Failure::new(
                code::covered_target,
                format!("that element is {reason}; observe again"),
            );
            failure.details = json!({"reason": reason});
            failure
        };
        let covered_by = |cover: &Value| {
            let mut failure = Failure::new(
                code::covered_target,
                format!(
                    "that element is covered by {}; observe again",
                    covering(cover)
                ),
            );
            failure.details = json!({"reason": "covered", "covered_by": cover});
            failure
        };
        if second["gone"] == true {
            return Err(stale());
        }
        if second["hidden"] == true {
            return Err(refused("hidden"));
        }
        if second["disabled"] == true {
            return Err(refused("disabled"));
        }
        if second["covered"] == true {
            return Err(match second.get("cover") {
                Some(cover) => covered_by(cover),
                None => refused("covered"),
            });
        }
        let moved = ["x", "y"].iter().any(|axis| {
            (first[axis].as_f64().unwrap_or(0.0) - second[axis].as_f64().unwrap_or(0.0)).abs() > 2.0
        });
        if moved {
            return Err(refused("moving"));
        }
        let (dx, dy) = self.driver.frame_offset(&frame)?;
        let (x, y) = (
            second["x"].as_f64().unwrap_or(0.0) + dx,
            second["y"].as_f64().unwrap_or(0.0) + dy,
        );
        // Not covered in its own document is not enough for an element in
        // a frame: the pages around the frame may draw over it.
        if !self.driver.reaches(&frame, x, y)? {
            let mut failure = refused("covered");
            failure.message =
                "that element is covered by the page around its frame; observe again".into();
            return Err(failure);
        }
        Ok(Located {
            x,
            y,
            kind: second["kind"].as_str().unwrap_or_default().to_string(),
            options: second["options"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|o| o.as_str().map(str::to_string))
                .collect(),
        })
    }

    fn tab(
        &mut self,
        action: TabAction,
        tab: Option<&str>,
        url: Option<&str>,
        audit: &mut Map<String, Value>,
    ) -> Result<Value, Failure> {
        audit.insert("action".into(), json!(format!("{action:?}").to_lowercase()));
        let need_tab = || required(tab, "name the tab, such as t2");
        match action {
            TabAction::New => {
                self.driver.new_tab(url)?;
            }
            TabAction::Switch => self.driver.switch_tab(need_tab()?)?,
            TabAction::Close => {
                let active = self.driver.active_tab();
                self.driver.close_tab(tab.unwrap_or(&active))?;
            }
        }
        let mut result = self.page(audit)?;
        result.insert("tabs".into(), json!(self.driver.tabs()?));
        if !self.driver.dialog_open() {
            self.read(None)?.into_json(&mut result);
        }
        Ok(Value::Object(result))
    }

    fn dialog(
        &mut self,
        accept: bool,
        text: Option<&str>,
        audit: &mut Map<String, Value>,
    ) -> Result<Value, Failure> {
        audit.insert(
            "action".into(),
            json!(if accept { "accept" } else { "dismiss" }),
        );
        self.driver.answer_dialog(accept, text)?;
        let events = self.settle()?;
        let before = Map::new();
        let mut result = Map::new();
        result.insert(
            "answered".into(),
            json!(if accept { "accept" } else { "dismiss" }),
        );
        self.after(&before, events, &mut result)?;
        let page = self.page(audit)?;
        result.extend(page);
        Ok(Value::Object(result))
    }

    /// The active tab's id, URL and title, and the audit's origin and path.
    fn page(&mut self, audit: &mut Map<String, Value>) -> Result<Map<String, Value>, Failure> {
        let tabs = self.driver.tabs()?;
        let active = tabs
            .iter()
            .find(|tab| tab.active)
            .cloned()
            .ok_or_else(|| Failure::new(code::browser_gone, "the session has no tab".into()))?;
        audit.insert("tab".into(), json!(active.tab));
        let (origin, path) = origin_and_path(&active.url);
        audit.insert("origin".into(), json!(origin));
        audit.insert("path".into(), json!(path));
        if !super::egress::shown(&active.url) {
            let failure = super::egress::not_shown(&active.url);
            self.driver.navigate("about:blank")?;
            return Err(failure);
        }
        let mut page = Map::new();
        page.insert("tab".into(), json!(active.tab));
        page.insert("url".into(), json!(active.url));
        page.insert("title".into(), json!(active.title));
        Ok(page)
    }

    fn main_frame(&mut self) -> Result<Frame, Failure> {
        self.driver
            .frames()?
            .into_iter()
            .next()
            .ok_or_else(|| Failure::new(code::browser_gone, "the tab has no frame".into()))
    }

    /// Reads the active tab into a new snapshot: every frame, or only the
    /// element `scope` and what is inside it.
    fn read(&mut self, scope: Option<(String, usize)>) -> Result<Reading, Failure> {
        self.next_snapshot += 1;
        let id = format!("o{}", self.next_snapshot);
        let frames = self.driver.frames()?;
        let tab = self.driver.active_tab();
        let scoped = match &scope {
            Some((snapshot, position)) => {
                let (frame, index) = self.reference(snapshot, *position)?;
                Some((snapshot.clone(), index, frame.id))
            }
            None => None,
        };
        let mut refs = Vec::new();
        let mut elements = Vec::new();
        let mut listed = Vec::new();
        // A scoped reading reads one frame, wherever it is in the tree.
        let read_at_most = if scoped.is_some() {
            frames.len()
        } else {
            MAX_FRAMES
        };
        let mut truncated = frames.len() > read_at_most;
        let mut out_of_time = None;
        for (n, frame) in frames.iter().take(read_at_most).enumerate() {
            let label = format!("f{n}");
            let parent = frame
                .parent
                .as_ref()
                .and_then(|parent| frames.iter().position(|f| &f.id == parent))
                .map(|at| format!("f{at}"));
            let left = MAX_ELEMENTS - refs.len();
            let expression = match &scoped {
                Some((snapshot, index, scope_frame)) => {
                    if &frame.id != scope_frame {
                        continue;
                    }
                    format!("__latch.read({id:?}, {left}, {snapshot:?}, {index})")
                }
                None => format!("__latch.read({id:?}, {left}, null, 0)"),
            };
            if self.read_until.is_some_and(|end| Instant::now() >= end) {
                truncated = true;
                out_of_time.get_or_insert_with(|| {
                    Failure::new(code::timeout, "the page did not answer in time".into())
                });
                break;
            }
            let read = match self.driver.run(frame, &expression) {
                Ok(read) => read,
                // A frame that did not answer in time is left out, and said so.
                Err(failure) if failure.code == "timeout" => {
                    truncated = true;
                    out_of_time = Some(failure);
                    continue;
                }
                // A frame that went away while being read is left out.
                Err(_) => continue,
            };
            if read["stale"] == true {
                return Err(stale());
            }
            let mut entry = json!({"frame": label, "origin": read["origin"], "url": read["url"]});
            if let Some(parent) = parent {
                entry["parent"] = json!(parent);
            }
            listed.push(entry);
            for (index, element) in read["elements"]
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
            {
                let identity = read["ids"][index].as_u64().ok_or_else(|| {
                    Failure::new(
                        code::engine_error,
                        "the page's reader listed an element with no identity".into(),
                    )
                })?;
                let key = (frame.id.clone(), frame.document.clone(), identity);
                let next = &mut self.next_number;
                let number = *self
                    .numbers
                    .entry(tab.clone())
                    .or_default()
                    .entry(key)
                    .or_insert_with(|| {
                        *next += 1;
                        *next
                    });
                let mut element = element.clone();
                element["ref"] = json!(format!("e{number}"));
                if n > 0 {
                    element["frame"] = json!(label);
                }
                refs.push(Ref {
                    frame: frame.clone(),
                    index,
                    number,
                    role: element["role"].as_str().unwrap_or_default().to_string(),
                    name: element["name"].as_str().unwrap_or_default().to_string(),
                });
                elements.push(element);
            }
            if read["more"] == true || refs.len() >= MAX_ELEMENTS {
                truncated = true;
            }
        }
        if listed.is_empty()
            && let Some(failure) = out_of_time
        {
            return Err(failure);
        }
        // A document gone from the tab takes its elements' numbers with it.
        if let Some(known) = self.numbers.get_mut(&tab) {
            known.retain(|(frame, document, _), _| {
                frames
                    .iter()
                    .any(|f| &f.id == frame && &f.document == document)
            });
        }
        self.snapshots.push_back(Snapshot {
            id: id.clone(),
            tab,
            refs,
        });
        if !self.quiet {
            self.last_shown = Some(id.clone());
        }
        while self.snapshots.len() > KEEP {
            self.snapshots.pop_front();
        }
        Ok(Reading {
            snapshot: id,
            elements,
            frames: listed,
            truncated,
        })
    }

    fn snapshot(&self, id: &str) -> Result<&Snapshot, Failure> {
        self.snapshots.iter().find(|s| s.id == id).ok_or_else(stale)
    }

    /// The frame and in-frame index of the element at `position` in `snapshot`.
    fn reference(&self, snapshot: &str, position: usize) -> Result<(Frame, usize), Failure> {
        let entry = self
            .snapshot(snapshot)?
            .refs
            .get(position)
            .ok_or_else(stale)?;
        Ok((entry.frame.clone(), entry.index))
    }

    /// The snapshot and position `target` names, checked against the page as
    /// it is now: same tab, same frame document.
    fn bound(&mut self, snapshot: Option<&str>, target: &str) -> Result<(String, usize), Failure> {
        let number = target
            .strip_prefix('e')
            .and_then(|n| n.parse::<usize>().ok())
            .filter(|n| *n > 0)
            .ok_or_else(|| {
                Failure::new(
                    code::bad_target,
                    format!("{target:?} is not a ref; refs look like e7"),
                )
            })?;
        let latest = self.last_shown.clone();
        let snapshot = match snapshot.or(latest.as_deref()) {
            Some(id) => self.snapshot(id)?,
            None => return Err(stale()),
        };
        let Some(position) = snapshot.refs.iter().position(|r| r.number == number) else {
            return Err(Failure::new(
                code::no_such_target,
                format!("snapshot {} has no {target}", snapshot.id),
            ));
        };
        let entry = &snapshot.refs[position];
        if snapshot.tab != self.driver.active_tab() {
            return Err(stale());
        }
        let (id, frame) = (snapshot.id.clone(), entry.frame.clone());
        let now = self.driver.frames()?;
        if !now
            .iter()
            .any(|f| f.id == frame.id && f.document == frame.document)
        {
            return Err(stale());
        }
        Ok((id, position))
    }

    /// The element as `snapshot` listed it, looked up again by the reader.
    fn element(&mut self, snapshot: &str, position: usize) -> Result<Value, Failure> {
        let (frame, index) = self.reference(snapshot, position)?;
        let read = self.driver.run(
            &frame,
            &format!("__latch.describeAt({snapshot:?}, {index})"),
        )?;
        if read.is_null() {
            return Err(stale());
        }
        Ok(read)
    }

    fn no_dialog(&self) -> Result<(), Failure> {
        if self.driver.dialog_open() {
            return Err(Failure::new(
                code::dialog_open,
                "a dialog is open; answer it with `dialog accept|dismiss`".into(),
            ));
        }
        Ok(())
    }
}

fn required<'a>(text: Option<&'a str>, message: &str) -> Result<&'a str, Failure> {
    text.ok_or_else(|| Failure::new(code::bad_arguments, message.to_string()))
}

fn scroll_delta(text: Option<&str>) -> Result<f64, Failure> {
    match text {
        None | Some("down") => Ok(SCROLL),
        Some("up") => Ok(-SCROLL),
        Some(other) => Err(Failure::new(
            code::bad_arguments,
            format!("scroll goes up or down, not {other:?}"),
        )),
    }
}

/// A URL's origin and path, for the audit: never its query or fragment.
pub fn origin_and_path(url: &str) -> (String, String) {
    let without = url.split(['?', '#']).next().unwrap_or_default();
    match without.split_once("://") {
        Some((scheme, rest)) => {
            let (host, path) = match rest.find('/') {
                Some(at) => (&rest[..at], &rest[at..]),
                None => (rest, "/"),
            };
            (format!("{scheme}://{host}"), path.to_string())
        }
        // about:blank, data: and the like have no origin to name.
        None => (
            without.split(':').next().unwrap_or_default().to_string() + ":",
            String::new(),
        ),
    }
}

/// What the page's reader says covers an element, in words: a frame by
/// its site, anything else by its role and name.
pub fn covering(cover: &Value) -> String {
    let (role, name) = (
        cover["role"].as_str().unwrap_or("generic"),
        cover["name"].as_str().unwrap_or_default(),
    );
    match cover["frame"].as_str() {
        Some("opaque") => "a frame".into(),
        Some(site) => format!("a frame from {site}"),
        None if name.is_empty() => format!("a {role} element"),
        None => format!("{role} {name:?}"),
    }
}

/// A frame's document, as a confirmation is bound to it.
fn document_key(frame: &Frame) -> String {
    format!("{}/{}", frame.id, frame.document)
}

fn stale() -> Failure {
    Failure::new(
        code::stale_snapshot,
        "that ref is from a page that has changed; observe again".into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::failure::Exit;

    #[test]
    fn the_audit_keeps_origin_and_path_but_never_the_query() {
        assert_eq!(
            origin_and_path("https://shop.example/cart/1?token=abc#top"),
            ("https://shop.example".into(), "/cart/1".into())
        );
        assert_eq!(
            origin_and_path("http://127.0.0.1:5000"),
            ("http://127.0.0.1:5000".into(), "/".into())
        );
        assert_eq!(
            origin_and_path("data:text/html,<p>secret</p>"),
            ("data:".into(), "".into())
        );
        assert_eq!(origin_and_path("about:blank"), ("about:".into(), "".into()));
    }

    #[test]
    fn calls_parse_from_the_hosts_lines() {
        let call: Call = serde_json::from_value(json!({
            "op": "act", "action": "click", "target": "e3", "snapshot": "o2"
        }))
        .unwrap();
        assert!(matches!(
            call,
            Call::Act {
                action: Action::Click,
                ..
            }
        ));
        assert!(
            serde_json::from_value::<Call>(json!({"op": "review", "target": "e3", "enter": true}))
                .is_ok()
        );
        assert!(serde_json::from_value::<Call>(json!({"op": "act", "action": "hover"})).is_err());
        assert_eq!(scroll_delta(Some("up")).unwrap(), -SCROLL);
        assert!(scroll_delta(Some("left")).is_err());
    }

    /// An engine with no optional features: only enough of the contract
    /// to say where it is, and to go somewhere else.
    struct Bare {
        url: String,
    }

    impl Driver for Bare {
        fn limit(&mut self, _: Option<Instant>) {}
        fn tabs(&mut self) -> Result<Vec<super::super::driver::Tab>, Failure> {
            Ok(vec![super::super::driver::Tab {
                tab: "t1".into(),
                url: self.url.clone(),
                title: "Cart".into(),
                opener: None,
                active: true,
            }])
        }
        fn capabilities(&self) -> Vec<String> {
            Vec::new()
        }
        fn dialog_open(&self) -> bool {
            false
        }
        fn active_tab(&self) -> String {
            "t1".into()
        }
        fn new_tab(&mut self, _: Option<&str>) -> Result<String, Failure> {
            unreachable!()
        }
        fn front(&mut self) -> Result<(), Failure> {
            unreachable!()
        }
        fn switch_tab(&mut self, _: &str) -> Result<(), Failure> {
            unreachable!()
        }
        fn close_tab(&mut self, _: &str) -> Result<(), Failure> {
            unreachable!()
        }
        fn navigate(&mut self, url: &str) -> Result<(), Failure> {
            self.url = url.to_string();
            Ok(())
        }
        fn back(&mut self) -> Result<bool, Failure> {
            unreachable!()
        }
        fn frames(&mut self) -> Result<Vec<Frame>, Failure> {
            unreachable!()
        }
        fn run(&mut self, _: &Frame, _: &str) -> Result<Value, Failure> {
            unreachable!()
        }
        fn call(&mut self, _: &Frame, _: &str, _: Vec<Value>) -> Result<Value, Failure> {
            unreachable!()
        }
        fn holds_focus(&mut self, _: &Frame) -> Result<bool, Failure> {
            Ok(true)
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
        fn settle(&mut self) -> Result<Vec<Event>, Failure> {
            unreachable!()
        }
        fn answer_dialog(&mut self, _: bool, _: Option<&str>) -> Result<(), Failure> {
            unreachable!()
        }
        fn eval(&mut self, _: &str) -> Result<Value, Failure> {
            unreachable!()
        }
        fn set_files(
            &mut self,
            _: &Frame,
            _: &str,
            _: &[std::path::PathBuf],
        ) -> Result<(), Failure> {
            unreachable!()
        }
        fn upload_folder(&self) -> std::path::PathBuf {
            unreachable!()
        }
        fn downloads(&mut self) -> Result<Vec<super::super::driver::Download>, Failure> {
            unreachable!()
        }
        fn download_file(
            &mut self,
            _: &str,
        ) -> Result<(super::super::driver::Download, std::path::PathBuf), Failure> {
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
    fn a_tab_a_page_took_off_the_web_is_never_read() {
        // Reading it would panic: the refusal comes first, and leaves it blank.
        let mut core = Core::new(Bare { url: String::new() });
        for (call, url, scheme) in [
            (
                Call::Text,
                "file:///Users/someone/latch/state/config.json",
                "file:",
            ),
            (Call::Screenshot, "data:text/html,hi", "data:"),
            (
                Call::Eval { script: "1".into() },
                "chrome://version/",
                "chrome:",
            ),
        ] {
            core.driver.url = url.into();
            let failure = core.call(call, None).err().unwrap();
            assert_eq!(
                (failure.exit, failure.code.as_str()),
                (Exit::Denied, "url_refused")
            );
            assert!(failure.message.contains(scheme), "{}", failure.message);
            assert_eq!(core.driver.url, "about:blank");
        }
    }

    #[test]
    fn a_missing_capability_is_refused_before_dispatch_and_audited() {
        // Every driver method that would dispatch panics: refusing must come first.
        let mut core = Core::new(Bare {
            url: "https://shop.example/cart?id=7".into(),
        });
        for (call, capability) in [
            (Call::Eval { script: "1".into() }, "eval"),
            (Call::Downloads, "downloads"),
            (
                Call::DownloadGet {
                    download: "d1".into(),
                },
                "downloads",
            ),
        ] {
            let failure = core.call(call, None).err().unwrap();
            assert_eq!(
                (failure.exit, failure.code.as_str()),
                (Exit::Failed, "unsupported_capability")
            );
            assert_eq!(failure.details["capability"], capability);
            assert_eq!(
                failure.details["audit"],
                json!({"capability": capability, "tab": "t1", "origin": "https://shop.example", "path": "/cart"})
            );
        }
    }

    /// A page of `frames` frames, each read taking `each`, as a page
    /// under heavy load answers; within its own limit every time.
    struct Slow {
        frames: usize,
        each: Duration,
    }

    impl Driver for Slow {
        fn front(&mut self) -> Result<(), Failure> {
            unreachable!()
        }
        fn limit(&mut self, _: Option<Instant>) {}
        fn tabs(&mut self) -> Result<Vec<super::super::driver::Tab>, Failure> {
            Bare {
                url: "https://shop.example/cart?id=7".into(),
            }
            .tabs()
        }
        fn active_tab(&self) -> String {
            "t1".into()
        }
        fn dialog_open(&self) -> bool {
            false
        }
        fn capabilities(&self) -> Vec<String> {
            Vec::new()
        }
        fn frames(&mut self) -> Result<Vec<Frame>, Failure> {
            Ok((0..self.frames)
                .map(|n| Frame {
                    id: format!("F{n}"),
                    parent: (n > 0).then(|| "F0".to_string()),
                    document: "D".into(),
                })
                .collect())
        }
        fn run(&mut self, _: &Frame, _: &str) -> Result<Value, Failure> {
            std::thread::sleep(self.each);
            Ok(
                json!({"origin": "https://shop.example", "url": "https://shop.example/", "elements": [], "text": "a page"}),
            )
        }
        fn settle(&mut self) -> Result<Vec<Event>, Failure> {
            Ok(Vec::new())
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
        fn call(&mut self, _: &Frame, _: &str, _: Vec<Value>) -> Result<Value, Failure> {
            unreachable!()
        }
        fn holds_focus(&mut self, _: &Frame) -> Result<bool, Failure> {
            Ok(true)
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
        fn set_files(
            &mut self,
            _: &Frame,
            _: &str,
            _: &[std::path::PathBuf],
        ) -> Result<(), Failure> {
            unreachable!()
        }
        fn upload_folder(&self) -> std::path::PathBuf {
            unreachable!()
        }
        fn downloads(&mut self) -> Result<Vec<super::super::driver::Download>, Failure> {
            unreachable!()
        }
        fn download_file(
            &mut self,
            _: &str,
        ) -> Result<(super::super::driver::Download, std::path::PathBuf), Failure> {
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
    fn a_reading_ends_with_its_calls_time_and_says_what_it_left_out() {
        // Each frame answers in its own time, but 40 of them take 2 s: a
        // call given 400 ms reads what fits and says the rest is missing.
        let mut core = Core::new(Slow {
            frames: 40,
            each: Duration::from_millis(50),
        });
        let started = Instant::now();
        core.limit(started + Duration::from_millis(400));
        let observed = core
            .call(
                Call::Observe {
                    tab: None,
                    scope: None,
                },
                None,
            )
            .unwrap()
            .result;
        // Soon after its 400 ms, on a slow machine too; reading on would take 2 s.
        let took = started.elapsed();
        assert!(took < Duration::from_secs(1), "took {took:?}");
        assert_eq!(observed["truncated"], true);
        let read = observed["frames"].as_array().unwrap().len();
        assert!((1..40).contains(&read), "{read} frames read");
    }

    #[test]
    fn a_snapshot_reads_at_most_max_frames() {
        let mut core = Core::new(Slow {
            frames: MAX_FRAMES + 30,
            each: Duration::ZERO,
        });
        core.limit(Instant::now() + CALL);
        let observed = core
            .call(
                Call::Observe {
                    tab: None,
                    scope: None,
                },
                None,
            )
            .unwrap()
            .result;
        assert_eq!(observed["truncated"], true);
        assert_eq!(observed["frames"].as_array().unwrap().len(), MAX_FRAMES);
    }
}

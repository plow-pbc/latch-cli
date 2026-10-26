//! The Chrome for Testing driver: the driver contract (`driver.rs`) over
//! CDP on the engine's private pipe.
//!
//! The browser is one session's: the window it starts with and every tab
//! opened from it (a page target whose opener is not one of its tabs is
//! never adopted), its dialogs, and its downloads, found from the frame
//! that started them.
//!
//! Each tab is a page target with a flattened CDP session. Cross-site and
//! sandboxed frames run in other processes, as targets of their own; they
//! are attached automatically (`Target.setAutoAttach`) and their sessions
//! kept by frame. latch's script runs in an isolated world it creates per
//! frame document. `Runtime.enable` is never sent.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::cdp::{self, Cdp, Pipes};
use super::driver::{self, Download, Driver, Event, Frame, HTTP_ERRORS, KEY_NAMES, Runtime, Tab};
use super::engine::{CFT, Engine};
use super::profile;
use crate::failure::{Failure, code};

/// How long the browser has to start, answer a command, or load a page.
const STARTUP: Duration = Duration::from_secs(30);
const COMMAND: Duration = Duration::from_secs(30);
/// How long a closing browser has before its process group is killed.
const CLOSING: Duration = Duration::from_secs(10);
/// How long after an input a navigation may still start, and how long a
/// started one may take before the call returns anyway.
const NAVIGATION_STARTS: Duration = Duration::from_millis(350);
const LOADING: Duration = Duration::from_secs(15);

/// The only variables the engine gets.
const PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

/// latch's page reader, defined once per document in its isolated world.
const READER: &str = include_str!("reader.js");

struct TabState {
    id: String,
    target: String,
    session: String,
    opener: Option<String>,
}

/// The session's tabs, and what happened in them since it last asked.
struct Share {
    tabs: Vec<TabState>,
    active: String,
    next_tab: u32,
    /// The tab a dialog is blocking.
    dialog: Option<String>,
    pending: Vec<Event>,
    mouse: (f64, f64),
    /// Its downloads, each with the guid its file is staged under.
    downloads: Vec<(String, Download)>,
}

/// How a command to one session ended.
enum Reply {
    Answer(Value),
    /// The caller's stop event came first; it stays queued.
    Stopped,
    /// The session went away first: Chrome may never answer it now.
    Gone,
}

/// A frame running in another process: its own CDP session, below `parent`.
struct Remote {
    parent: String,
    frame: String,
}

/// Where a frame lives: its session, and the session of the document that
/// holds its `<iframe>` element.
#[derive(Clone)]
struct Placement {
    session: String,
    owner_session: Option<String>,
    parent: Option<String>,
    document: String,
}

/// A session's browser.
pub struct Cft {
    child: Child,
    cdp: Cdp,
    share: Share,
    remotes: HashMap<String, Remote>,
    placements: HashMap<String, Placement>,
    /// Each CDP session's root frame.
    roots: HashMap<String, String>,
    /// Isolated worlds by (frame, document).
    worlds: HashMap<(String, String), i64>,
    /// Page targets whose opener is not one of the session's tabs, with
    /// that opener: adopted only once their opener turns out to be one.
    unassigned: Vec<(String, Option<String>)>,
    /// The runtime's folder: the engine's home, downloads staged under
    /// their guid, and uploads.
    scratch: PathBuf,
    /// When the call under way must have its answer (`Driver::limit`).
    deadline: Option<Instant>,
    /// Whether every request is paused for the guards (`Driver::guard`).
    pausing: bool,
}

impl Cft {
    /// Starts the engine on the session's profile in folder `profile`; with
    /// `headless`, in Chrome's headless mode, with no window; with
    /// `page_isolation_off`, without Chrome's own sandbox, which the host
    /// cannot run (`sandbox::page_isolation_off`).
    pub fn launch(
        state_dir: &Path,
        profile: &Path,
        scratch: &Path,
        headless: bool,
        display: Option<&str>,
        proxy: &str,
        page_isolation_off: bool,
    ) -> Result<Self, Failure> {
        if CFT.driver_contract != driver::CONTRACT {
            return Err(Failure::new(
                code::engine_unverified,
                format!(
                    "the {} plugin speaks driver contract {}, this latch {}",
                    CFT.engine,
                    CFT.driver_contract,
                    driver::CONTRACT
                ),
            ));
        }
        let engine = CFT.verified(state_dir)?;
        let profile = profile::prepare(profile, &CFT.engine)?;
        let Pipes {
            chrome_reads,
            chrome_writes,
            cdp,
        } = cdp::pipes().map_err(io_failure)?;
        let child = start(
            &engine,
            &profile.data(),
            scratch,
            headless,
            display,
            proxy,
            page_isolation_off,
            &chrome_reads,
            &chrome_writes,
        )?;
        // Only Chrome holds its ends now, so its exit closes the pipe.
        drop((chrome_reads, chrome_writes));
        let mut browser = Cft {
            child,
            cdp,
            share: Share {
                tabs: Vec::new(),
                active: String::new(),
                next_tab: 0,
                dialog: None,
                pending: Vec::new(),
                mouse: (0.0, 0.0),
                downloads: Vec::new(),
            },
            remotes: HashMap::new(),
            placements: HashMap::new(),
            roots: HashMap::new(),
            worlds: HashMap::new(),
            unassigned: Vec::new(),
            scratch: scratch.to_path_buf(),
            deadline: None,
            pausing: false,
        };
        let opened = browser.listen().and_then(|()| {
            let target = browser.new_window(browser.by(COMMAND))?;
            browser.adopt(&target, None)
        });
        match opened {
            Ok(tab) => {
                browser.share.active = tab;
                Ok(browser)
            }
            Err(failure) => {
                browser.close();
                Err(failure)
            }
        }
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// From now on hears about every new tab and every download. Chrome
    /// starts with no window (`--no-startup-window`); the session opens its
    /// own, behind every other window. Every download lands
    /// in the staging folder under its own guid, never opened.
    fn listen(&mut self) -> Result<(), Failure> {
        let deadline = Instant::now() + STARTUP;
        self.cdp.call(
            "Target.setDiscoverTargets",
            json!({"discover": true}),
            None,
            deadline,
        )?;
        // Every tab's and frame's loads of a scheme off the web stop before
        // they start, in any tab and however they came about; `cdp` fails
        // each one it hears of (`Fetch.requestPaused`).
        self.cdp
            .call("Fetch.enable", fetch_patterns(false), None, deadline)?;
        let folder = self.scratch.join("downloads");
        fs::create_dir_all(&folder).map_err(io_failure)?;
        self.cdp.call(
            "Browser.setDownloadBehavior",
            json!({"behavior": "allowAndName", "downloadPath": folder, "eventsEnabled": true}),
            None,
            deadline,
        )?;
        Ok(())
    }

    /// Pauses every request while the guard stands, and only loads off the
    /// web otherwise.
    fn pause_for_guards(&mut self) -> Result<(), Failure> {
        let guarding = !self.cdp.guards.is_empty();
        if guarding != self.pausing {
            let deadline = self.by(COMMAND);
            self.cdp
                .call("Fetch.enable", fetch_patterns(guarding), None, deadline)?;
            self.pausing = guarding;
        }
        Ok(())
    }

    /// Whether page target `target` is one of the session's tabs.
    fn has_target(&self, target: &str) -> bool {
        self.share.tabs.iter().any(|tab| tab.target == target)
    }

    /// Whether CDP session `session` is one of the session's tabs, or holds
    /// a frame of one.
    fn has_session(&self, session: &str) -> bool {
        let mut at = Some(session.to_string());
        while let Some(session) = at {
            if self.share.tabs.iter().any(|tab| tab.session == session) {
                return true;
            }
            at = self
                .remotes
                .get(&session)
                .map(|remote| remote.parent.clone());
        }
        false
    }

    /// Whether a frame's page is the session's: a tab, or held by one, in
    /// the same process or another. `dialogs` are the CDP sessions whose
    /// page opened a dialog that latch has heard of but not yet taken in.
    fn has_frame(&mut self, frame: &str, dialogs: &[&str]) -> Result<bool, Failure> {
        if self.has_target(frame) {
            return Ok(true);
        }
        let session = self
            .remotes
            .iter()
            .find(|(_, remote)| remote.frame == frame)
            .map(|(session, _)| session.clone())
            .or_else(|| self.placements.get(frame).map(|p| p.session.clone()));
        if session.is_some_and(|session| self.has_session(&session)) {
            return Ok(true);
        }
        // A frame latch has not read yet: look for it in each tab's tree.
        let tabs: Vec<String> = self
            .share
            .tabs
            .iter()
            .map(|tab| tab.session.clone())
            .collect();
        for session in tabs {
            // A tab a dialog holds answers for none of its frames until the
            // dialog is answered; it is left out, not waited on.
            if self.held_by_dialog(&session) || dialogs.contains(&session.as_str()) {
                continue;
            }
            let here = [session.clone()];
            let opened = |event: &Value| dialog_in(event, &here);
            if let Ok(Reply::Answer(tree)) = self.session_call(
                "Page.getFrameTree",
                json!({}),
                &session,
                self.by(COMMAND),
                opened,
            ) && holds_frame(&tree["frameTree"], frame)
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Whether a dialog holds the tab whose CDP session `session` is.
    fn held_by_dialog(&self, session: &str) -> bool {
        self.share.dialog.as_ref().is_some_and(|tab| {
            self.share
                .tabs
                .iter()
                .any(|t| &t.id == tab && t.session == session)
        })
    }

    /// The CDP sessions of the session's tabs: where its dialogs are
    /// reported.
    fn tab_sessions(&self) -> Vec<String> {
        self.share
            .tabs
            .iter()
            .map(|tab| tab.session.clone())
            .collect()
    }

    /// A download starts or moves on. One that can't be traced to a page of
    /// the session is never listed.
    /// `dialogs` as for `owner_of_frame`.
    fn download_event(
        &mut self,
        method: &str,
        params: &Value,
        dialogs: &[&str],
    ) -> Result<(), Failure> {
        let guid = params["guid"].as_str().unwrap_or_default().to_string();
        if method == "Browser.downloadWillBegin" {
            let frame = params["frameId"].as_str().unwrap_or_default();
            if !self.has_frame(frame, dialogs)? {
                return Ok(());
            }
            let share = &mut self.share;
            let download = Download {
                download: format!("d{}", share.downloads.len() + 1),
                name: params["suggestedFilename"]
                    .as_str()
                    .unwrap_or("download")
                    .to_string(),
                url: params["url"].as_str().unwrap_or_default().to_string(),
                state: "in_progress".into(),
                bytes: 0,
                total: None,
            };
            share.pending.push(Event::Download {
                download: download.download.clone(),
                name: download.name.clone(),
                state: download.state.clone(),
            });
            share.downloads.push((guid, download));
            return Ok(());
        }
        let share = &mut self.share;
        if let Some((_, download)) = share.downloads.iter_mut().find(|(id, _)| *id == guid) {
            download.bytes = params["receivedBytes"].as_u64().unwrap_or(download.bytes);
            download.total = params["totalBytes"].as_u64().filter(|total| *total > 0);
            let state = match params["state"].as_str().unwrap_or("inProgress") {
                "completed" => "completed",
                "canceled" => "canceled",
                _ => "in_progress",
            };
            if state != download.state {
                download.state = state.into();
                share.pending.push(Event::Download {
                    download: download.download.clone(),
                    name: download.name.clone(),
                    state: state.into(),
                });
            }
        }
        Ok(())
    }

    /// A new window, a tab of its own. It opens behind every other window,
    /// its page still visible, so the owner's front app and desktop stay as
    /// they are.
    fn new_window(&mut self, deadline: Instant) -> Result<String, Failure> {
        let created = self.cdp.call(
            "Target.createTarget",
            json!({"url": "about:blank", "newWindow": true, "background": true}),
            None,
            deadline,
        )?;
        Ok(created["targetId"].as_str().unwrap_or_default().to_string())
    }

    /// Attaches to page target `target` as a new tab of the session.
    fn adopt(&mut self, target: &str, opener: Option<String>) -> Result<String, Failure> {
        let deadline = self.by(COMMAND);
        let attached = self.cdp.call(
            "Target.attachToTarget",
            json!({"targetId": target, "flatten": true}),
            None,
            deadline,
        )?;
        let session = attached["sessionId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        self.cdp
            .call("Page.enable", json!({}), Some(&session), deadline)?;
        self.cdp
            .call("Network.enable", json!({}), Some(&session), deadline)?;
        if !self.auto_attach(&session)? {
            return Err(Failure::new(
                code::engine_error,
                "the tab closed as it opened".into(),
            ));
        }
        let share = &mut self.share;
        share.next_tab += 1;
        let id = format!("t{}", share.next_tab);
        share.tabs.push(TabState {
            id: id.clone(),
            target: target.to_string(),
            session,
            opener,
        });
        Ok(id)
    }

    /// Has Chrome attach the frames that `session`'s page runs in other
    /// processes. False when the session went away first.
    fn auto_attach(&mut self, session: &str) -> Result<bool, Failure> {
        let reply = self.session_call(
            "Target.setAutoAttach",
            json!({"autoAttach": true, "waitForDebuggerOnStart": false, "flatten": true}),
            session,
            self.by(COMMAND),
            |_| false,
        )?;
        Ok(matches!(reply, Reply::Answer(_)))
    }

    /// Sends `method` to `session` and waits for its reply, or for an event
    /// `stop` accepts. Pages add and drop frames all the time (trackers,
    /// ads), and a frame's session can go away while latch waits for it;
    /// Chrome then may never answer, so the session's detach ends the wait.
    fn session_call(
        &mut self,
        method: &str,
        params: Value,
        session: &str,
        deadline: Instant,
        stop: impl Fn(&Value) -> bool,
    ) -> Result<Reply, Failure> {
        let id = self.cdp.send(method, params, Some(session))?;
        let gone = std::cell::Cell::new(false);
        let reply = self.cdp.reply_or(id, method, deadline, |event| {
            if event["method"] == "Target.detachedFromTarget"
                && event["params"]["sessionId"] == session
            {
                gone.set(true);
                return true;
            }
            stop(event)
        })?;
        Ok(match reply {
            Some(value) => Reply::Answer(value),
            None if gone.get() => Reply::Gone,
            None => Reply::Stopped,
        })
    }

    /// One command to a frame's session; a frame that went away, or whose
    /// session Chrome no longer knows, is stale.
    fn frame_command(
        &mut self,
        method: &str,
        params: Value,
        session: &str,
        deadline: Instant,
    ) -> Result<Value, Failure> {
        match self.session_call(method, params, session, deadline, |_| false) {
            Ok(Reply::Answer(value)) => Ok(value),
            Ok(_) => Err(stale()),
            Err(failure) if failure.code == "engine_error" => Err(stale()),
            Err(failure) => Err(failure),
        }
    }

    /// When a wait of `budget` from now must end: sooner, if the call
    /// under way has less time left.
    fn by(&self, budget: Duration) -> Instant {
        let own = Instant::now() + budget;
        self.deadline.map_or(own, |deadline| deadline.min(own))
    }

    fn tab(&self, id: &str) -> Result<&TabState, Failure> {
        self.share
            .tabs
            .iter()
            .find(|tab| tab.id == id)
            .ok_or_else(|| Failure::new(code::no_such_tab, format!("no tab {id}")))
    }

    fn active_state(&self) -> &TabState {
        self.tab(&self.share.active).expect("the active tab exists")
    }

    /// The tab whose page `session` is, or holds as one of its frames.
    fn tab_of(&self, session: &str) -> Option<String> {
        let mut session = session;
        loop {
            if let Some(tab) = self.share.tabs.iter().find(|tab| tab.session == session) {
                return Some(tab.id.clone());
            }
            session = &self.remotes.get(session)?.parent;
        }
    }

    /// Takes in the events read so far: frames attached in other
    /// processes, tabs pages opened or closed, dialogs and downloads, each
    /// for the session whose page it is. Returns them.
    fn absorb(&mut self) -> Result<Vec<Value>, Failure> {
        let events = self.cdp.take_events();
        let mut attach = Vec::new();
        // Pages that opened a dialog anywhere in this batch: a download
        // heard before it must not wait on that page either.
        let dialogs: Vec<&str> = events
            .iter()
            .filter(|event| event["method"] == "Page.javascriptDialogOpening")
            .filter_map(|event| event["sessionId"].as_str())
            .collect();
        for event in &events {
            let params = &event["params"];
            let session = event["sessionId"].as_str().unwrap_or_default();
            match event["method"].as_str().unwrap_or_default() {
                "Target.attachedToTarget" if params["targetInfo"]["type"] == "iframe" => {
                    let child = params["sessionId"].as_str().unwrap_or_default().to_string();
                    let frame = params["targetInfo"]["targetId"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string();
                    self.remotes.insert(
                        child.clone(),
                        Remote {
                            parent: session.to_string(),
                            frame,
                        },
                    );
                    attach.push(child);
                }
                "Target.detachedFromTarget" => {
                    if let Some(session) = params["sessionId"].as_str() {
                        self.remotes.remove(session);
                    }
                }
                "Target.targetCreated" if params["targetInfo"]["type"] == "page" => {
                    let target = params["targetInfo"]["targetId"]
                        .as_str()
                        .unwrap_or_default();
                    if !self.has_target(target) && !self.unassigned.iter().any(|(t, _)| t == target)
                    {
                        let opener = params["targetInfo"]["openerId"]
                            .as_str()
                            .map(str::to_string);
                        self.unassigned.push((target.to_string(), opener));
                    }
                }
                "Target.targetDestroyed" => {
                    let target = params["targetId"].as_str().unwrap_or_default();
                    self.unassigned.retain(|(t, _)| t != target);
                    let share = &mut self.share;
                    if let Some(at) = share.tabs.iter().position(|tab| tab.target == target) {
                        let gone = share.tabs.remove(at);
                        share.pending.push(Event::TabClosed {
                            tab: gone.id.clone(),
                        });
                        if share.dialog.as_deref() == Some(gone.id.as_str()) {
                            share.dialog = None;
                        }
                        if share.active == gone.id
                            && let Some(last) = share.tabs.last()
                        {
                            share.active = last.id.clone();
                        }
                    }
                }
                "Page.javascriptDialogOpening" => {
                    let share = &mut self.share;
                    {
                        if let Some(tab) = share.tabs.iter().find(|tab| tab.session == session) {
                            share.dialog = Some(tab.id.clone());
                            share.pending.push(Event::Dialog {
                                tab: tab.id.clone(),
                                kind: params["type"].as_str().unwrap_or("alert").to_string(),
                                message: clip(params["message"].as_str().unwrap_or_default(), 300),
                            });
                        }
                    }
                }
                "Page.javascriptDialogClosed" => {
                    let share = &mut self.share;
                    {
                        let blocked = share
                            .dialog
                            .as_ref()
                            .and_then(|id| share.tabs.iter().find(|tab| &tab.id == id));
                        if blocked.is_some_and(|tab| tab.session == session) {
                            share.dialog = None;
                        }
                    }
                }
                method @ ("Browser.downloadWillBegin" | "Browser.downloadProgress") => {
                    self.download_event(method, params, &dialogs)?;
                }
                // A page's own load, XHR or fetch the site refused (the
                // only responses kept), in any of its frames: its status
                // and origin, never its path or query.
                "Network.responseReceived" => {
                    let response = &params["response"];
                    let status = response["status"].as_u64().unwrap_or(0);
                    let origin = url::Url::parse(response["url"].as_str().unwrap_or_default())
                        .map(|url| url.origin().ascii_serialization());
                    let refused = self
                        .share
                        .pending
                        .iter()
                        .filter(|e| matches!(e, Event::HttpError { .. }))
                        .count();
                    if let (400..=599, Ok(origin), Some(tab)) =
                        (status, origin, self.tab_of(session))
                        && refused < HTTP_ERRORS
                    {
                        let retry_after = response["headers"]
                            .as_object()
                            .into_iter()
                            .flatten()
                            .find(|(name, _)| name.eq_ignore_ascii_case("retry-after"))
                            .and_then(|(_, value)| value.as_str())
                            .map(|value| clip(value, 40));
                        self.share.pending.push(Event::HttpError {
                            tab,
                            status: status as u16,
                            origin,
                            retry_after,
                        });
                    }
                }
                _ => {}
            }
        }
        for session in attach {
            // Frames inside that frame's process are attached the same way.
            // The frame may be gone again by the time latch asks: then it is
            // simply no longer there.
            if !self.remotes.contains_key(&session) {
                continue;
            }
            // Its own requests are watched as its page's are; one the frame
            // no longer answers simply goes unwatched.
            let deadline = self.by(COMMAND);
            let _ = self.session_call("Network.enable", json!({}), &session, deadline, |_| false);
            match self.auto_attach(&session) {
                Ok(true) => {}
                Ok(false) => {
                    self.remotes.remove(&session);
                }
                Err(failure) if failure.code == "engine_error" => {
                    self.remotes.remove(&session);
                }
                // Out of the call's time: the frame stays, read as it is,
                // without the frames in its own process attached.
                Err(failure) if failure.code == "timeout" => {}
                Err(failure) => return Err(failure),
            }
        }
        // A page one of the session's tabs opened is the session's tab.
        for (target, opener) in std::mem::take(&mut self.unassigned) {
            if !opener
                .as_deref()
                .is_some_and(|opener| self.has_target(opener))
            {
                self.unassigned.push((target, opener));
                continue;
            }
            let opener_tab = self
                .share
                .tabs
                .iter()
                .find(|tab| Some(&tab.target) == opener.as_ref())
                .map(|tab| tab.id.clone());
            // A tab that closed as fast as it opened is no tab.
            let id = match self.adopt(&target, opener_tab.clone()) {
                Ok(id) => id,
                Err(failure) if failure.code == "engine_error" => continue,
                Err(failure) => return Err(failure),
            };
            let url = match self.target_info(&target) {
                Ok((url, _)) => url,
                Err(failure) if failure.code == "engine_error" => String::new(),
                Err(failure) => return Err(failure),
            };
            self.share.pending.push(Event::NewTab {
                tab: id,
                url,
                opener: opener_tab,
            });
        }
        Ok(events)
    }

    fn target_info(&mut self, target: &str) -> Result<(String, String), Failure> {
        let info = self.cdp.call(
            "Target.getTargetInfo",
            json!({"targetId": target}),
            None,
            self.by(COMMAND),
        )?;
        let info = &info["targetInfo"];
        Ok((
            info["url"].as_str().unwrap_or_default().to_string(),
            info["title"].as_str().unwrap_or_default().to_string(),
        ))
    }

    /// Sends one input event to the active tab and waits for it to be
    /// handled, or for a dialog it opened.
    fn input(&mut self, method: &str, params: Value) -> Result<(), Failure> {
        let session = self.active_state().session.clone();
        let id = self.cdp.send(method, params, Some(&session))?;
        let here = [session];
        let opened = |event: &Value| dialog_in(event, &here);
        self.cdp.reply_or(id, method, self.by(COMMAND), opened)?;
        Ok(())
    }

    fn key(
        &mut self,
        kind: &str,
        key: &Key,
        modifiers: i64,
        commands: &[&str],
    ) -> Result<(), Failure> {
        let mut params = json!({
            "type": kind,
            "key": key.key,
            "code": key.code,
            "windowsVirtualKeyCode": key.key_code,
            "modifiers": modifiers,
        });
        if kind == "keyDown" && !key.text.is_empty() {
            params["text"] = json!(key.text);
            params["unmodifiedText"] = json!(key.text);
        }
        if !commands.is_empty() {
            params["commands"] = json!(commands);
        }
        self.input("Input.dispatchKeyEvent", params)
    }

    /// The frames of the active tab's tree in one session, depth first.
    fn frame_tree(
        &mut self,
        session: &str,
        owner_session: Option<&str>,
        frames: &mut Vec<Frame>,
    ) -> Result<(), Failure> {
        // A page held by a dialog does not answer for its frames until the
        // dialog is answered: one open already, or one opening meanwhile.
        let blocked = || {
            Failure::new(
                code::dialog_open,
                "a dialog is open; answer it with `dialog accept|dismiss`".into(),
            )
        };
        if self.held_by_dialog(session) {
            return Err(blocked());
        }
        let here = [session.to_string()];
        let opened = |event: &Value| dialog_in(event, &here);
        let tree = match self.session_call(
            "Page.getFrameTree",
            json!({}),
            session,
            self.by(COMMAND),
            opened,
        ) {
            Ok(Reply::Answer(tree)) => tree,
            Ok(Reply::Stopped) => return Err(blocked()),
            Ok(Reply::Gone) => return Err(stale()),
            Err(failure) if failure.code == "engine_error" => return Err(stale()),
            Err(failure) => return Err(failure),
        };
        let mut stack = vec![(tree["frameTree"].clone(), true)];
        while let Some((node, root)) = stack.pop() {
            let frame = &node["frame"];
            let id = frame["id"].as_str().unwrap_or_default().to_string();
            if root {
                self.roots.insert(session.to_string(), id.clone());
            }
            self.placements.insert(
                id.clone(),
                Placement {
                    session: session.to_string(),
                    owner_session: if root {
                        owner_session.map(str::to_string)
                    } else {
                        Some(session.to_string())
                    },
                    parent: frame["parentId"].as_str().map(str::to_string),
                    document: frame["loaderId"].as_str().unwrap_or_default().to_string(),
                },
            );
            frames.push(Frame {
                id,
                parent: frame["parentId"].as_str().map(str::to_string),
                document: frame["loaderId"].as_str().unwrap_or_default().to_string(),
            });
            let children = node["childFrames"].as_array().cloned().unwrap_or_default();
            for child in children.into_iter().rev() {
                stack.push((child, false));
            }
        }
        Ok(())
    }

    /// Runs latch's script in an isolated world of `frame` and returns the
    /// result as CDP's remote object: its JSON `value`, or an `objectId`.
    fn evaluate(
        &mut self,
        frame: &Frame,
        expression: &str,
        by_value: bool,
        wait: Duration,
    ) -> Result<Value, Failure> {
        let Some(placement) = self.placements.get(&frame.id).cloned() else {
            return Err(stale());
        };
        // A frame its page removed answers nothing: don't wait for it.
        let removed = |event: &Value| {
            event["method"] == "Page.frameDetached"
                && event["params"]["frameId"] == frame.id.as_str()
                && event["params"]["reason"] != "swap"
        };
        let key = (frame.id.clone(), frame.document.clone());
        let deadline = self.by(wait);
        for _ in 0..2 {
            if self.cdp.removed(&frame.id) {
                return Err(stale());
            }
            let context = match self.worlds.get(&key) {
                Some(context) => *context,
                None => {
                    let world = match self.session_call(
                        "Page.createIsolatedWorld",
                        json!({"frameId": frame.id, "worldName": "latch", "grantUniveralAccess": false}),
                        &placement.session,
                        deadline,
                        removed,
                    ) {
                        Ok(Reply::Answer(world)) => world,
                        Err(failure) if failure.code == "timeout" => return Err(failure),
                        _ => return Err(stale()),
                    };
                    let context = world["executionContextId"].as_i64().ok_or_else(stale)?;
                    self.worlds.insert(key.clone(), context);
                    context
                }
            };
            let here = self.tab_sessions();
            let opened = |event: &Value| dialog_in(event, &here) || removed(event);
            match self.session_call(
                "Runtime.evaluate",
                json!({
                    "expression": format!("{READER}\n{expression}"),
                    "contextId": context,
                    "returnByValue": by_value,
                    "awaitPromise": true,
                }),
                &placement.session,
                deadline,
                opened,
            ) {
                Ok(Reply::Answer(reply)) => {
                    if let Some(details) = reply.get("exceptionDetails") {
                        let text = details["exception"]["description"]
                            .as_str()
                            .or(details["text"].as_str())
                            .unwrap_or("failed");
                        return Err(Failure::new(
                            code::engine_error,
                            format!("latch's page script: {}", clip(text, 200)),
                        ));
                    }
                    return Ok(reply["result"].clone());
                }
                Ok(Reply::Stopped) if self.cdp.removed(&frame.id) => return Err(stale()),
                Ok(Reply::Stopped) => {
                    return Err(Failure::new(
                        code::dialog_open,
                        "a dialog is open; answer it with `dialog accept|dismiss`".into(),
                    ));
                }
                Ok(Reply::Gone) => return Err(stale()),
                // The document went away under its world: try a new one once,
                // then let the caller handle the replaced document.
                Err(failure) if failure.code == "engine_error" => {
                    self.worlds.remove(&key);
                }
                Err(failure) => return Err(failure),
            }
        }
        Err(stale())
    }

    /// Loads `url` in the active tab and waits for its load event.
    fn load(&mut self, url: &str) -> Result<(), Failure> {
        let deadline = self.by(COMMAND);
        let session = self.active_state().session.clone();
        self.absorb()?;
        let navigated = self.cdp.call(
            "Page.navigate",
            json!({"url": url}),
            Some(&session),
            deadline,
        )?;
        if let Some(error) = navigated["errorText"].as_str() {
            return Err(Failure::new(code::navigation_failed, error.to_string()));
        }
        // A page that opens a dialog while loading never fires its load event.
        let loaded = |event: &Value| {
            event["sessionId"] == session.as_str()
                && (event["method"] == "Page.loadEventFired"
                    || event["method"] == "Page.javascriptDialogOpening")
        };
        loop {
            self.cdp.pump(Duration::from_millis(100))?;
            let events = self.absorb()?;
            if events.iter().any(loaded) {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(Failure::new(
                    code::timeout,
                    format!("{url} did not finish loading"),
                ));
            }
        }
    }
}

impl Runtime for Cft {
    /// While a guard stands every request waits for latch, so the worker
    /// reads Chrome between calls too.
    fn tend(&mut self) -> bool {
        if self.cdp.guards.is_empty() {
            return false;
        }
        // A wait of zero reads nothing new from the pipe; a millisecond does.
        let _ = self.cdp.pump(Duration::from_millis(1));
        true
    }

    fn unanswered(&mut self) -> bool {
        self.cdp.take_unanswered()
    }
}

/// What `Fetch.enable` pauses: loads of a scheme off the web, and while a
/// guard stands (`Driver::guard`) every request, for `cdp` to check.
fn fetch_patterns(guarding: bool) -> Value {
    let mut patterns: Vec<Value> = cdp::OFF_THE_WEB
        .iter()
        .map(|scheme| json!({"urlPattern": format!("{scheme}:*")}))
        .collect();
    if guarding {
        patterns.push(json!({"urlPattern": "*", "requestStage": "Request"}));
    }
    json!({"patterns": patterns})
}

impl Cft {
    /// Whether each parent document of `frame` has that frame's own
    /// `<iframe>` at the point (`reaches`), not something of its own drawn
    /// over it; or, with none, as its active element (`holds_focus`).
    fn owners_hold(&mut self, frame: &Frame, at: Option<(f64, f64)>) -> Result<bool, Failure> {
        let deadline = self.by(COMMAND);
        let mut current = frame.clone();
        while let Some(parent_id) = current.parent.clone() {
            let (Some(place), Some(parent)) = (
                self.placements.get(&current.id).cloned(),
                self.placements.get(&parent_id).cloned(),
            ) else {
                return Err(stale());
            };
            let owner_session = place.owner_session.ok_or_else(stale)?;
            let parent_frame = Frame {
                id: parent_id,
                parent: parent.parent,
                document: parent.document,
            };
            let expression = match at {
                Some((x, y)) => {
                    let (dx, dy) = self.frame_offset(&parent_frame)?;
                    format!("document.elementFromPoint({}, {})", x - dx, y - dy)
                }
                None => "document.activeElement".to_string(),
            };
            let hit = self.evaluate(&parent_frame, &expression, false, COMMAND)?;
            let Some(object) = hit["objectId"].as_str().map(str::to_string) else {
                return Ok(false);
            };
            let node = self.frame_command(
                "DOM.describeNode",
                json!({"objectId": object}),
                &parent.session,
                deadline,
            );
            let _ = self.frame_command(
                "Runtime.releaseObject",
                json!({"objectId": object}),
                &parent.session,
                deadline,
            );
            let owner = self.frame_command(
                "DOM.getFrameOwner",
                json!({"frameId": current.id}),
                &owner_session,
                deadline,
            );
            let (Ok(node), Ok(owner)) = (node, owner) else {
                return Err(stale());
            };
            if node["node"]["backendNodeId"] != owner["backendNodeId"] {
                return Ok(false);
            }
            current = parent_frame;
        }
        Ok(true)
    }
}

impl Driver for Cft {
    fn limit(&mut self, deadline: Option<Instant>) {
        self.deadline = deadline;
    }
    fn guard(&mut self, values: &[String]) -> Result<(), Failure> {
        self.cdp.guards = values.to_vec();
        self.pause_for_guards()
    }
    fn guard_stopped(&mut self) -> bool {
        std::mem::take(&mut self.cdp.stopped)
    }

    fn tabs(&mut self) -> Result<Vec<Tab>, Failure> {
        self.cdp.pump(Duration::ZERO)?;
        self.absorb()?;
        let ids: Vec<(String, String, Option<String>)> = self
            .share
            .tabs
            .iter()
            .map(|tab| (tab.id.clone(), tab.target.clone(), tab.opener.clone()))
            .collect();
        let mut tabs = Vec::new();
        for (tab, target, opener) in ids {
            let (url, title) = self.target_info(&target)?;
            tabs.push(Tab {
                active: tab == self.share.active,
                tab,
                url,
                title,
                opener,
            });
        }
        Ok(tabs)
    }

    fn new_tab(&mut self, url: Option<&str>) -> Result<String, Failure> {
        let target = self.new_window(self.by(COMMAND))?;
        let tab = self.adopt(&target, None)?;
        self.share.active = tab.clone();
        if let Some(url) = url {
            self.load(url)?;
        }
        Ok(tab)
    }

    /// Makes `tab` the one the agent acts on. Nothing comes to the front
    /// (`Target.activateTarget` would take the owner's focus): a tab latch
    /// opened is its own window, and a tab a page opened is the visible one
    /// in its opener's window.
    fn switch_tab(&mut self, tab: &str) -> Result<(), Failure> {
        self.tab(tab)?;
        self.share.active = tab.to_string();
        Ok(())
    }

    /// The session's active tab to the front: the owner is to act in it.
    fn front(&mut self) -> Result<(), Failure> {
        let target = self.tab(&self.share.active.clone())?.target.clone();
        self.cdp.call(
            "Target.activateTarget",
            json!({"targetId": target}),
            None,
            self.by(COMMAND),
        )?;
        Ok(())
    }

    fn close_tab(&mut self, tab: &str) -> Result<(), Failure> {
        let target = self.tab(tab)?.target.clone();
        if self.share.tabs.len() == 1 {
            return Err(Failure::new(
                code::last_tab,
                "a session keeps at least one tab; close the session instead".into(),
            ));
        }
        self.cdp.call(
            "Target.closeTarget",
            json!({"targetId": target}),
            None,
            self.by(COMMAND),
        )?;
        let share = &mut self.share;
        share.tabs.retain(|state| state.id != tab);
        if share.dialog.as_deref() == Some(tab) {
            share.dialog = None;
        }
        if share.active == tab {
            let last = share.tabs.last().expect("one tab left").id.clone();
            self.switch_tab(&last)?;
        }
        Ok(())
    }

    fn active_tab(&self) -> String {
        self.share.active.clone()
    }

    fn navigate(&mut self, url: &str) -> Result<(), Failure> {
        self.load(url)
    }

    fn back(&mut self) -> Result<bool, Failure> {
        let session = self.active_state().session.clone();
        let deadline = self.by(COMMAND);
        let history = self.cdp.call(
            "Page.getNavigationHistory",
            json!({}),
            Some(&session),
            deadline,
        )?;
        let current = history["currentIndex"].as_u64().unwrap_or(0) as usize;
        if current == 0 {
            return Ok(false);
        }
        let entry = history["entries"][current - 1]["id"].clone();
        self.cdp.call(
            "Page.navigateToHistoryEntry",
            json!({"entryId": entry}),
            Some(&session),
            deadline,
        )?;
        Ok(true)
    }

    fn frames(&mut self) -> Result<Vec<Frame>, Failure> {
        self.cdp.pump(Duration::ZERO)?;
        self.absorb()?;
        let main = self.active_state().session.clone();
        let mut frames = Vec::new();
        self.frame_tree(&main, None, &mut frames)?;
        // Frames in other processes, below a session of this tab.
        let mut sessions = vec![main];
        let mut at = 0;
        while at < sessions.len() {
            let parent = sessions[at].clone();
            let children: Vec<(String, String)> = self
                .remotes
                .iter()
                .filter(|(_, remote)| remote.parent == parent)
                .map(|(session, remote)| (session.clone(), remote.frame.clone()))
                .collect();
            for (session, _) in children {
                let mut remote_frames = Vec::new();
                // A frame that just went away answers nothing; skip it.
                if self
                    .frame_tree(&session, Some(&parent), &mut remote_frames)
                    .is_ok()
                {
                    frames.extend(remote_frames);
                    sessions.push(session);
                }
            }
            at += 1;
        }
        Ok(tree_order(frames))
    }

    fn run(&mut self, frame: &Frame, expression: &str) -> Result<Value, Failure> {
        self.evaluate(frame, expression, true, COMMAND)
            .map(|object| object["value"].clone())
    }

    fn call(&mut self, frame: &Frame, function: &str, args: Vec<Value>) -> Result<Value, Failure> {
        // The reader is defined, and its world made, by an ordinary run.
        self.evaluate(frame, "true", true, COMMAND)?;
        let key = (frame.id.clone(), frame.document.clone());
        let (Some(context), Some(placement)) = (
            self.worlds.get(&key).copied(),
            self.placements.get(&frame.id).cloned(),
        ) else {
            return Err(stale());
        };
        // The arguments travel as CDP call arguments, never inside the
        // script's text, and the answer is a plain JSON value.
        let arguments: Vec<Value> = args
            .into_iter()
            .map(|value| json!({"value": value}))
            .collect();
        let here = self.tab_sessions();
        let opened = |event: &Value| dialog_in(event, &here);
        match self.session_call(
            "Runtime.callFunctionOn",
            json!({
                "functionDeclaration": function,
                "executionContextId": context,
                "arguments": arguments,
                "returnByValue": true,
            }),
            &placement.session,
            self.by(COMMAND),
            opened,
        ) {
            Ok(Reply::Answer(reply)) if reply.get("exceptionDetails").is_some() => Err(
                Failure::new(code::engine_error, "latch's page script failed".into()),
            ),
            Ok(Reply::Answer(reply)) => Ok(reply["result"]["value"].clone()),
            Ok(Reply::Stopped) => Err(Failure::new(
                code::dialog_open,
                "a dialog is open; answer it with `dialog accept|dismiss`".into(),
            )),
            Ok(Reply::Gone) | Err(_) => Err(stale()),
        }
    }

    fn reaches(&mut self, frame: &Frame, x: f64, y: f64) -> Result<bool, Failure> {
        self.owners_hold(frame, Some((x, y)))
    }

    fn holds_focus(&mut self, frame: &Frame) -> Result<bool, Failure> {
        self.owners_hold(frame, None)
    }

    fn frame_offset(&mut self, frame: &Frame) -> Result<(f64, f64), Failure> {
        let mut offset = (0.0, 0.0);
        let mut current = frame.clone();
        let deadline = self.by(COMMAND);
        loop {
            let Some(placement) = self.placements.get(&current.id).cloned() else {
                return Err(stale());
            };
            let Some(owner_session) = placement.owner_session else {
                return Ok(offset);
            };
            let owner = self
                .frame_command(
                    "DOM.getFrameOwner",
                    json!({"frameId": current.id}),
                    &owner_session,
                    deadline,
                )
                .map_err(|_| stale())?;
            let model = self
                .frame_command(
                    "DOM.getBoxModel",
                    json!({"backendNodeId": owner["backendNodeId"]}),
                    &owner_session,
                    deadline,
                )
                .map_err(|_| stale())?;
            let content = &model["model"]["content"];
            offset.0 += content[0].as_f64().unwrap_or(0.0);
            offset.1 += content[1].as_f64().unwrap_or(0.0);
            // Box coordinates are relative to the owner session's own root
            // frame; add where that frame sits.
            let Some(root) = self.roots.get(&owner_session).cloned() else {
                return Err(stale());
            };
            current = Frame {
                id: root,
                parent: None,
                document: String::new(),
            };
        }
    }

    fn click(&mut self, x: f64, y: f64) -> Result<(), Failure> {
        let (from_x, from_y) = self.share.mouse;
        for step in 1..=4 {
            let t = f64::from(step) / 4.0;
            self.input(
                "Input.dispatchMouseEvent",
                json!({"type": "mouseMoved", "x": from_x + (x - from_x) * t, "y": from_y + (y - from_y) * t}),
            )?;
            pause(8, 16);
        }
        self.share.mouse = (x, y);
        let press =
            |kind: &str| json!({"type": kind, "x": x, "y": y, "button": "left", "clickCount": 1});
        self.input("Input.dispatchMouseEvent", press("mousePressed"))?;
        pause(40, 90);
        self.input("Input.dispatchMouseEvent", press("mouseReleased"))
    }

    fn type_text(&mut self, text: &str) -> Result<(), Failure> {
        for ch in text.chars() {
            if self.share.dialog.is_some() {
                break;
            }
            if ch == '\n' {
                self.press("Enter")?;
            } else {
                let text = ch.to_string();
                let (code, key_code) = physical_key(ch);
                let key = Key {
                    key: &text,
                    code: &code,
                    key_code,
                    text: &text,
                };
                self.key("keyDown", &key, 0, &[])?;
                self.key("keyUp", &key, 0, &[])?;
            }
            pause(25, 70);
            self.cdp.pump(Duration::ZERO)?;
            self.absorb()?;
        }
        Ok(())
    }

    fn select_all(&mut self) -> Result<(), Failure> {
        let key = Key {
            key: "a",
            code: "KeyA",
            key_code: 65,
            text: "",
        };
        // Command on macOS, Control elsewhere; the command does the work.
        let modifiers = if cfg!(target_os = "macos") { 4 } else { 2 };
        self.key("keyDown", &key, modifiers, &["selectAll"])?;
        self.key("keyUp", &key, modifiers, &[])
    }

    fn press(&mut self, name: &str) -> Result<(), Failure> {
        let key = named_key(name).ok_or_else(|| {
            Failure::new(
                code::bad_key,
                format!("unknown key {name:?}; keys: {}", KEY_NAMES.join(", ")),
            )
        })?;
        self.key("keyDown", &key, 0, &[])?;
        pause(30, 60);
        self.key("keyUp", &key, 0, &[])
    }

    fn wheel(&mut self, x: f64, y: f64, delta_y: f64) -> Result<(), Failure> {
        self.input(
            "Input.dispatchMouseEvent",
            json!({"type": "mouseWheel", "x": x, "y": y, "deltaX": 0, "deltaY": delta_y}),
        )
    }

    fn settle(&mut self) -> Result<Vec<Event>, Failure> {
        let main = self.active_state().target.clone();
        let started = |event: &Value| {
            event["params"]["frameId"] == main.as_str()
                && (event["method"] == "Page.frameStartedLoading"
                    || event["method"] == "Page.frameRequestedNavigation")
        };
        let stopped = |event: &Value| {
            event["params"]["frameId"] == main.as_str()
                && event["method"] == "Page.frameStoppedLoading"
        };
        let deadline = self.by(LOADING);
        // Use the page's event loop: its delayed input effects can run
        // after a wait of the same length on the host has already ended.
        if self.share.dialog.is_none()
            && let Some(placement) = self.placements.get(&main).cloned()
        {
            let frame = Frame {
                id: main.clone(),
                parent: None,
                document: placement.document,
            };
            let timer = format!(
                "new Promise(resolve => setTimeout(resolve, {}))",
                NAVIGATION_STARTS.as_millis()
            );
            match self.evaluate(&frame, &timer, true, LOADING) {
                Ok(_) => {}
                Err(failure)
                    if matches!(failure.code.as_str(), "stale_snapshot" | "dialog_open") => {}
                Err(failure) => return Err(failure),
            }
        }
        let mut events = self.absorb()?;
        let loading = events.iter().any(started) && !events.iter().any(stopped);
        if loading {
            while self.share.dialog.is_none() && Instant::now() < deadline {
                self.cdp.pump(Duration::from_millis(100))?;
                events = self.absorb()?;
                if events.iter().any(stopped) {
                    break;
                }
            }
        }
        self.cdp.pump(Duration::from_millis(150))?;
        self.absorb()?;
        Ok(std::mem::take(&mut self.share.pending))
    }

    fn dialog_open(&self) -> bool {
        self.share.dialog.is_some()
    }

    fn answer_dialog(&mut self, accept: bool, text: Option<&str>) -> Result<(), Failure> {
        self.cdp.pump(Duration::ZERO)?;
        self.absorb()?;
        let Some(tab) = self.share.dialog.clone() else {
            return Err(Failure::new(code::no_dialog, "no dialog is open".into()));
        };
        let session = self.tab(&tab)?.session.clone();
        let mut params = json!({"accept": accept});
        if let Some(text) = text {
            params["promptText"] = json!(text);
        }
        self.cdp.call(
            "Page.handleJavaScriptDialog",
            params,
            Some(&session),
            self.by(COMMAND),
        )?;
        self.share.dialog = None;
        Ok(())
    }

    fn eval(&mut self, script: &str) -> Result<Value, Failure> {
        let session = self.active_state().session.clone();
        let id = self.cdp.send(
            "Runtime.evaluate",
            json!({"expression": script, "returnByValue": true, "awaitPromise": true}),
            Some(&session),
        )?;
        let here = [session];
        let opened = |event: &Value| dialog_in(event, &here);
        let reply = self
            .cdp
            .reply_or(id, "Runtime.evaluate", self.by(COMMAND), opened)?;
        let Some(reply) = reply else {
            return Err(Failure::new(
                code::dialog_open,
                "the script opened a dialog; answer it with `dialog accept|dismiss`".into(),
            ));
        };
        if let Some(details) = reply.get("exceptionDetails") {
            let text = details["exception"]["description"]
                .as_str()
                .or(details["text"].as_str())
                .unwrap_or("the script threw");
            return Err(Failure::new(code::eval_failed, clip(text, 1000)));
        }
        Ok(reply["result"]["value"].clone())
    }

    fn set_files(
        &mut self,
        frame: &Frame,
        element: &str,
        files: &[PathBuf],
    ) -> Result<(), Failure> {
        let object = self.evaluate(frame, element, false, COMMAND)?;
        let Some(object) = object["objectId"].as_str() else {
            return Err(stale());
        };
        let session = self
            .placements
            .get(&frame.id)
            .ok_or_else(stale)?
            .session
            .clone();
        self.cdp.call(
            "DOM.setFileInputFiles",
            json!({"files": files, "objectId": object}),
            Some(&session),
            self.by(COMMAND),
        )?;
        Ok(())
    }

    fn upload_folder(&self) -> PathBuf {
        self.scratch.join("uploads")
    }

    fn downloads(&mut self) -> Result<Vec<Download>, Failure> {
        self.cdp.pump(Duration::ZERO)?;
        self.absorb()?;
        Ok(self
            .share
            .downloads
            .iter()
            .map(|(_, d)| d.clone())
            .collect())
    }

    fn download_file(&mut self, id: &str) -> Result<(Download, PathBuf), Failure> {
        let deadline = self.by(LOADING);
        loop {
            self.cdp.pump(Duration::from_millis(100))?;
            self.absorb()?;
            let Some((guid, download)) =
                self.share.downloads.iter().find(|(_, d)| d.download == id)
            else {
                return Err(Failure::new(
                    code::no_such_download,
                    format!("no download {id}"),
                ));
            };
            match download.state.as_str() {
                "completed" => {
                    return Ok((download.clone(), self.scratch.join("downloads").join(guid)));
                }
                "canceled" => {
                    return Err(Failure::new(
                        code::download_failed,
                        format!("download {id} was canceled"),
                    ));
                }
                _ if Instant::now() >= deadline => {
                    return Err(Failure::new(
                        code::download_incomplete,
                        format!("download {id} is still in progress; ask again later"),
                    ));
                }
                _ => {}
            }
        }
    }

    fn capabilities(&self) -> Vec<String> {
        CFT.capabilities.clone()
    }

    /// The page as the engine draws it, and how its pixels map to the
    /// page's CSS coordinates: pixel = (css - scroll) * device_pixel_ratio.
    fn screenshot(&mut self) -> Result<(Value, Vec<u8>), Failure> {
        let deadline = self.by(COMMAND);
        let session = self.active_state().session.clone();
        let session = Some(session.as_str());
        let metrics = self
            .cdp
            .call("Page.getLayoutMetrics", json!({}), session, deadline)?;
        let shot = self.cdp.call(
            "Page.captureScreenshot",
            json!({"format": "png"}),
            session,
            deadline,
        )?;
        let png = shot["data"]
            .as_str()
            .and_then(cdp::base64_decode)
            .filter(|png| png.starts_with(b"\x89PNG\r\n\x1a\n") && png.len() >= 24)
            .ok_or_else(|| Failure::new(code::engine_error, "no screenshot".into()))?;
        let width = u32::from_be_bytes(png[16..20].try_into().expect("4 bytes"));
        let height = u32::from_be_bytes(png[20..24].try_into().expect("4 bytes"));
        let viewport = &metrics["cssVisualViewport"];
        let css_width = viewport["clientWidth"].as_f64().unwrap_or(0.0);
        let ratio = if css_width > 0.0 {
            f64::from(width) / css_width
        } else {
            1.0
        };
        let meta = json!({
            "size": png.len(),
            "width": width,
            "height": height,
            "mapping": {
                "css_width": css_width,
                "css_height": viewport["clientHeight"],
                "scroll_x": viewport["pageX"],
                "scroll_y": viewport["pageY"],
                "device_pixel_ratio": ratio,
            },
        });
        Ok((meta, png))
    }

    /// Asks Chrome to close, waits for it, then kills whatever is left of
    /// its process group and waits until the group is empty.
    fn close(&mut self) -> Value {
        let deadline = Instant::now() + CLOSING;
        let _ = self.cdp.call("Browser.close", json!({}), None, deadline);
        let mut forced = false;
        while self.child.try_wait().ok().flatten().is_none() {
            if Instant::now() >= deadline {
                forced = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let group = self.child.id() as libc::pid_t;
        // SAFETY: signals only; Chrome leads its own process group.
        unsafe { libc::killpg(group, libc::SIGKILL) };
        let _ = self.child.wait();
        let gone_by = Instant::now() + CLOSING;
        // A group with no process left answers ESRCH.
        // SAFETY: signal 0 only checks whether the group still exists.
        let exists = || unsafe { libc::killpg(group, 0) } == 0;
        while exists() && Instant::now() < gone_by {
            std::thread::sleep(Duration::from_millis(50));
        }
        let left = exists();
        json!({"closed": !left, "forced": forced})
    }
}

/// Frames reordered so each comes right after its parent's earlier
/// siblings' subtrees: the main frame first, then depth first.
fn tree_order(frames: Vec<Frame>) -> Vec<Frame> {
    let Some(main) = frames.first().cloned() else {
        return frames;
    };
    let mut ordered = Vec::with_capacity(frames.len());
    let mut stack = vec![main];
    while let Some(frame) = stack.pop() {
        let children: Vec<Frame> = frames
            .iter()
            .filter(|f| f.parent.as_deref() == Some(frame.id.as_str()))
            .cloned()
            .collect();
        ordered.push(frame);
        stack.extend(children.into_iter().rev());
    }
    // A frame whose parent is not listed (it was going away) still counts.
    for frame in frames {
        if !ordered.iter().any(|f| f.id == frame.id) {
            ordered.push(frame);
        }
    }
    ordered
}

struct Key<'a> {
    key: &'a str,
    code: &'a str,
    key_code: i64,
    text: &'a str,
}

fn named_key(name: &str) -> Option<Key<'static>> {
    let (key, code, key_code, text) = match name {
        "Enter" => ("Enter", "Enter", 13, "\r"),
        "Tab" => ("Tab", "Tab", 9, ""),
        "Escape" => ("Escape", "Escape", 27, ""),
        "Backspace" => ("Backspace", "Backspace", 8, ""),
        "Delete" => ("Delete", "Delete", 46, ""),
        "Space" => (" ", "Space", 32, " "),
        "ArrowUp" => ("ArrowUp", "ArrowUp", 38, ""),
        "ArrowDown" => ("ArrowDown", "ArrowDown", 40, ""),
        "ArrowLeft" => ("ArrowLeft", "ArrowLeft", 37, ""),
        "ArrowRight" => ("ArrowRight", "ArrowRight", 39, ""),
        "Home" => ("Home", "Home", 36, ""),
        "End" => ("End", "End", 35, ""),
        "PageUp" => ("PageUp", "PageUp", 33, ""),
        "PageDown" => ("PageDown", "PageDown", 34, ""),
        _ => return None,
    };
    Some(Key {
        key,
        code,
        key_code,
        text,
    })
}

/// The physical key (`code`) and key code a US keyboard sends `ch` with,
/// which pages read to tell a digit or letter from other keys; none for
/// other characters.
fn physical_key(ch: char) -> (String, i64) {
    match ch {
        '0'..='9' => (format!("Digit{ch}"), ch as i64),
        'a'..='z' | 'A'..='Z' => {
            let upper = ch.to_ascii_uppercase();
            (format!("Key{upper}"), upper as i64)
        }
        ' ' => ("Space".into(), 32),
        _ => (String::new(), 0),
    }
}

/// A person's pause between two inputs.
fn pause(min_ms: u64, max_ms: u64) {
    std::thread::sleep(Duration::from_millis(rand::random_range(min_ms..max_ms)));
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    text.chars().take(max - 1).collect::<String>() + "…"
}

/// Whether `event` is a dialog opening in a page with one of `sessions`.
fn dialog_in(event: &Value, sessions: &[String]) -> bool {
    event["method"] == "Page.javascriptDialogOpening"
        && sessions.iter().any(|s| event["sessionId"] == s.as_str())
}

/// Whether frame `id` is in a `Page.getFrameTree` tree.
fn holds_frame(tree: &Value, id: &str) -> bool {
    tree["frame"]["id"] == id
        || tree["childFrames"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|child| holds_frame(child, id))
}

fn stale() -> Failure {
    Failure::new(
        code::stale_snapshot,
        "that element's document was replaced; observe again".into(),
    )
}

/// Starts the engine on its profile, headed unless `headless`, with the CDP
/// pipe as its fd 3 (commands in) and fd 4 (replies out), in a process
/// group of its own.
#[allow(clippy::too_many_arguments)]
fn start(
    engine: &Engine,
    data: &Path,
    scratch: &Path,
    headless: bool,
    display: Option<&str>,
    proxy: &str,
    page_isolation_off: bool,
    reads: &impl AsRawFd,
    writes: &impl AsRawFd,
) -> Result<Child, Failure> {
    for folder in ["tmp", "crash"] {
        fs::create_dir_all(scratch.join(folder)).map_err(io_failure)?;
    }
    let log = File::create(scratch.join("engine.log")).map_err(io_failure)?;
    let (reads, writes) = (reads.as_raw_fd(), writes.as_raw_fd());
    let mut command = Command::new(&engine.executable);
    command.args(&engine.recipe);
    // Chrome's headless mode (the full browser, with no window). The pin's
    // recipe may never carry it; only the host's setting adds it here.
    if headless {
        command.arg("--headless");
    }
    // Likewise only on the host's word: where its AppArmor keeps Chrome's
    // sandbox from starting, Chrome runs in latch's alone.
    if page_isolation_off {
        command.arg("--no-sandbox");
    }
    // Every request through the worker's proxy, loopback ones too (Chrome
    // sends those direct unless told not to), and WebRTC with it.
    command
        .arg(format!("--proxy-server={proxy}"))
        .arg("--proxy-bypass-list=<-loopback>")
        .arg("--force-webrtc-ip-handling-policy=disable_non_proxied_udp")
        .arg(format!("--user-data-dir={}", data.display()))
        .env_clear()
        .env("PATH", PATH)
        .env("HOME", scratch)
        .env("TMPDIR", scratch.join("tmp"))
        // Chrome's crash reporter writes here instead of the user's Library.
        .env("BREAKPAD_DUMP_LOCATION", scratch.join("crash"))
        .current_dir(scratch)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log)
        .process_group(0);
    if let Some(display) = display {
        command.env("DISPLAY", display);
    }
    // SAFETY: between fork and exec, only async-signal-safe calls: the two
    // ends are first copied above fd 4 (so neither is overwritten by the
    // other's move), then onto 3 and 4, which exec keeps open.
    unsafe {
        command.pre_exec(move || {
            let high_reads = libc::fcntl(reads, libc::F_DUPFD_CLOEXEC, 10);
            let high_writes = libc::fcntl(writes, libc::F_DUPFD_CLOEXEC, 10);
            if high_reads < 0
                || high_writes < 0
                || libc::dup2(high_reads, 3) < 0
                || libc::dup2(high_writes, 4) < 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn().map_err(|err| {
        Failure::new(
            code::engine_failed,
            format!("{}: {err}", engine.executable.display()),
        )
    })?;
    super::worker::started_group(child.id());
    Ok(child)
}

pub fn io_failure(err: io::Error) -> Failure {
    Failure::new(code::engine_failed, err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(id: &str, parent: Option<&str>) -> Frame {
        Frame {
            id: id.into(),
            parent: parent.map(Into::into),
            document: String::new(),
        }
    }

    #[test]
    fn frames_from_other_processes_slot_in_below_their_parents() {
        // The main tree first, then a cross-site frame read from its own session.
        let frames = vec![
            frame("main", None),
            frame("same", Some("main")),
            frame("inner", Some("same")),
            frame("cross", Some("main")),
            frame("cross-child", Some("cross")),
        ];
        let order: Vec<String> = tree_order(frames).into_iter().map(|f| f.id).collect();
        assert_eq!(order, ["main", "same", "inner", "cross", "cross-child"]);
        let orphan = vec![frame("main", None), frame("lost", Some("gone"))];
        assert_eq!(tree_order(orphan).len(), 2);
    }

    #[test]
    fn every_listed_key_is_known_and_others_are_not() {
        for name in KEY_NAMES {
            assert!(named_key(name).is_some(), "{name}");
        }
        assert!(named_key("F13").is_none());
        assert_eq!(named_key("Enter").unwrap().text, "\r");
    }
}

//! The Chrome for Testing driver: the driver contract (`driver.rs`) over
//! CDP on the engine's private pipe.
//!
//! One browser serves every session on its profile. Each session's share
//! is the windows it opened and every tab opened from them (a page target
//! whose opener is no session's tab is never adopted), its dialogs, and its
//! downloads, found from the frame that started them; the driver contract
//! acts for one session at a time (`Seat`).
//!
//! Each tab is a page target with a flattened CDP session. Cross-site and
//! sandboxed frames run in other processes, as targets of their own; they
//! are attached automatically (`Target.setAutoAttach`) and their sessions
//! kept by frame. latch's script runs in an isolated world it creates per
//! frame document. `Runtime.enable` is never sent.

use std::cell::{RefCell, RefMut};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::cdp::{self, Cdp, Pipes};
use super::driver::{self, Download, Driver, Event, Frame, Tab};
use super::engine::{CFT, Engine, Focus};
use super::profile;
use crate::failure::{Exit, Failure};

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

/// One session's share of the browser: the windows it opened, every tab
/// opened from them, and what happened in them since it last asked.
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
    /// Its own scratch folder, where its uploads are staged.
    folder: PathBuf,
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

/// One profile's browser, shared by the sessions on it. The driver
/// contract acts for one session at a time (`select`), on that session's
/// own tabs, downloads and dialogs.
pub struct Cft {
    child: Child,
    cdp: Cdp,
    /// Each session's share, by the worker's key for the session.
    shares: HashMap<String, Share>,
    /// The session the driver contract's calls act for.
    current: String,
    remotes: HashMap<String, Remote>,
    placements: HashMap<String, Placement>,
    /// Each session's root frame.
    roots: HashMap<String, String>,
    /// Isolated worlds by (frame, document).
    worlds: HashMap<(String, String), i64>,
    /// Page targets whose opener is no session's tab, with that opener:
    /// never adopted from whichever window is in use, only once their
    /// opener turns out to be a session's.
    unassigned: Vec<(String, Option<String>)>,
    /// The runtime's folder: the engine's home, downloads staged under
    /// their guid, and a folder per session.
    scratch: PathBuf,
    _lock: profile::Lock,
}

impl Cft {
    pub fn launch(state_dir: &Path, profile_name: &str, scratch: &Path) -> Result<Self, Failure> {
        if CFT.driver_contract != driver::CONTRACT {
            return Err(Failure::failed(
                "engine_unverified",
                format!(
                    "the {} plugin speaks driver contract {}, this latch {}",
                    CFT.engine,
                    CFT.driver_contract,
                    driver::CONTRACT
                ),
            ));
        }
        let profile = profile::open(state_dir, profile_name, &CFT.engine)?;
        let lock = profile::lock(&profile)?;
        let engine = CFT.verified(state_dir)?;
        let Pipes {
            chrome_reads,
            chrome_writes,
            cdp,
        } = cdp::pipes().map_err(io_failure)?;
        let child = start(
            &engine,
            &profile.data(),
            scratch,
            &chrome_reads,
            &chrome_writes,
        )?;
        // Only Chrome holds its ends now, so its exit closes the pipe.
        drop((chrome_reads, chrome_writes));
        let mut browser = Cft {
            child,
            cdp,
            shares: HashMap::new(),
            current: String::new(),
            remotes: HashMap::new(),
            placements: HashMap::new(),
            roots: HashMap::new(),
            worlds: HashMap::new(),
            unassigned: Vec::new(),
            scratch: scratch.to_path_buf(),
            _lock: lock,
        };
        if let Err(failure) = browser.listen() {
            browser.close();
            return Err(failure);
        }
        Ok(browser)
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// From now on hears about every new tab and every download. Chrome
    /// starts with no window (`--no-startup-window`); each session opens
    /// its own (`begin`), as the pinned focus says. Every download lands
    /// in the staging folder under its own guid, never opened.
    fn listen(&mut self) -> Result<(), Failure> {
        let deadline = Instant::now() + STARTUP;
        self.cdp.call(
            "Target.setDiscoverTargets",
            json!({"discover": true}),
            None,
            deadline,
        )?;
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

    /// Starts session `key` with a window of its own, and makes it the one
    /// the driver contract acts for.
    pub fn begin(&mut self, key: &str) -> Result<(), Failure> {
        let folder = self.scratch.join("sessions").join(key);
        fs::create_dir_all(&folder).map_err(io_failure)?;
        self.shares.insert(
            key.to_string(),
            Share {
                tabs: Vec::new(),
                active: String::new(),
                next_tab: 0,
                dialog: None,
                pending: Vec::new(),
                mouse: (0.0, 0.0),
                downloads: Vec::new(),
                folder,
            },
        );
        self.select(key);
        let opened = self
            .new_window(Instant::now() + COMMAND)
            .and_then(|target| self.adopt(key, &target, None));
        match opened {
            Ok(tab) => {
                self.share_mut().active = tab;
                Ok(())
            }
            Err(failure) => {
                self.end(key);
                Err(failure)
            }
        }
    }

    /// Ends session `key`: closes every tab it has, and forgets its
    /// downloads (their files too) and its folder. The others go on.
    pub fn end(&mut self, key: &str) {
        let Some(share) = self.shares.remove(key) else {
            return;
        };
        for tab in &share.tabs {
            let _ = self.cdp.call(
                "Target.closeTarget",
                json!({"targetId": tab.target}),
                None,
                Instant::now() + COMMAND,
            );
        }
        for (guid, _) in &share.downloads {
            let _ = fs::remove_file(self.scratch.join("downloads").join(guid));
        }
        let _ = fs::remove_dir_all(&share.folder);
    }

    /// Makes session `key` the one the driver contract acts for.
    pub fn select(&mut self, key: &str) {
        self.current = key.to_string();
    }

    fn share(&self) -> &Share {
        self.shares
            .get(&self.current)
            .expect("the selected session has a share")
    }

    fn share_mut(&mut self) -> &mut Share {
        self.shares
            .get_mut(&self.current)
            .expect("the selected session has a share")
    }

    /// The session whose tab page target `target` is.
    fn owner_of_target(&self, target: &str) -> Option<String> {
        self.shares
            .iter()
            .find(|(_, share)| share.tabs.iter().any(|tab| tab.target == target))
            .map(|(key, _)| key.clone())
    }

    /// The session whose tab's CDP session `session` is, or holds the frame
    /// whose CDP session it is.
    fn owner_of_session(&self, session: &str) -> Option<String> {
        let mut at = Some(session.to_string());
        while let Some(session) = at {
            if let Some((key, _)) = self
                .shares
                .iter()
                .find(|(_, share)| share.tabs.iter().any(|tab| tab.session == session))
            {
                return Some(key.clone());
            }
            at = self
                .remotes
                .get(&session)
                .map(|remote| remote.parent.clone());
        }
        None
    }

    /// The session a frame's page belongs to: the tab it is, or the tab
    /// that holds it, in the same process or another. None when no tab of
    /// any session holds it.
    fn owner_of_frame(&mut self, frame: &str) -> Result<Option<String>, Failure> {
        if let Some(key) = self.owner_of_target(frame) {
            return Ok(Some(key));
        }
        let session = self
            .remotes
            .iter()
            .find(|(_, remote)| remote.frame == frame)
            .map(|(session, _)| session.clone())
            .or_else(|| self.placements.get(frame).map(|p| p.session.clone()));
        if let Some(key) = session.and_then(|session| self.owner_of_session(&session)) {
            return Ok(Some(key));
        }
        // A frame latch has not read yet: look for it in each tab's tree.
        let tabs: Vec<(String, String)> = self
            .shares
            .iter()
            .flat_map(|(key, share)| {
                share
                    .tabs
                    .iter()
                    .map(move |tab| (key.clone(), tab.session.clone()))
            })
            .collect();
        for (key, session) in tabs {
            let deadline = Instant::now() + COMMAND;
            if let Ok(tree) = self.frame_command("Page.getFrameTree", json!({}), &session, deadline)
                && holds_frame(&tree["frameTree"], frame)
            {
                return Ok(Some(key));
            }
        }
        Ok(None)
    }

    /// The CDP sessions of the selected session's tabs: where its dialogs
    /// are reported.
    fn tab_sessions(&self) -> Vec<String> {
        self.share()
            .tabs
            .iter()
            .map(|tab| tab.session.clone())
            .collect()
    }

    /// A download starts or moves on. It belongs to the session whose page
    /// started it; one that can't be traced to a session is never listed.
    fn download_event(&mut self, method: &str, params: &Value) -> Result<(), Failure> {
        let guid = params["guid"].as_str().unwrap_or_default().to_string();
        if method == "Browser.downloadWillBegin" {
            let frame = params["frameId"].as_str().unwrap_or_default();
            let Some(key) = self.owner_of_frame(frame)? else {
                return Ok(());
            };
            let share = self.shares.get_mut(&key).expect("the owner has a share");
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
        for share in self.shares.values_mut() {
            let Some((_, download)) = share.downloads.iter_mut().find(|(id, _)| *id == guid) else {
                continue;
            };
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

    /// A new window, a tab of its own. With the pinned focus `background`
    /// it opens behind every other window, its page still visible, so the
    /// owner's front app and desktop stay as they are.
    fn new_window(&mut self, deadline: Instant) -> Result<String, Failure> {
        let background = CFT.focus == Focus::Background;
        let created = self.cdp.call(
            "Target.createTarget",
            json!({"url": "about:blank", "newWindow": true, "background": background}),
            None,
            deadline,
        )?;
        Ok(created["targetId"].as_str().unwrap_or_default().to_string())
    }

    /// Attaches to page target `target` as a new tab of session `key`.
    fn adopt(
        &mut self,
        key: &str,
        target: &str,
        opener: Option<String>,
    ) -> Result<String, Failure> {
        let deadline = Instant::now() + COMMAND;
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
        if !self.auto_attach(&session)? {
            return Err(Failure::failed(
                "engine_error",
                "the tab closed as it opened".into(),
            ));
        }
        let share = self.shares.get_mut(key).expect("the session has a share");
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
            Instant::now() + COMMAND,
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

    fn tab(&self, id: &str) -> Result<&TabState, Failure> {
        self.share()
            .tabs
            .iter()
            .find(|tab| tab.id == id)
            .ok_or_else(|| Failure::usage("no_such_tab", format!("no tab {id}")))
    }

    fn active_state(&self) -> &TabState {
        self.tab(&self.share().active)
            .expect("the active tab exists")
    }

    /// Takes in the events read so far: frames attached in other
    /// processes, tabs pages opened or closed, dialogs and downloads, each
    /// for the session whose page it is. Returns them.
    fn absorb(&mut self) -> Result<Vec<Value>, Failure> {
        let events = self.cdp.take_events();
        let mut attach = Vec::new();
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
                    if self.owner_of_target(target).is_none()
                        && !self.unassigned.iter().any(|(t, _)| t == target)
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
                    for share in self.shares.values_mut() {
                        let Some(at) = share.tabs.iter().position(|tab| tab.target == target)
                        else {
                            continue;
                        };
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
                    for share in self.shares.values_mut() {
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
                    for share in self.shares.values_mut() {
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
                    self.download_event(method, params)?;
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
            match self.auto_attach(&session) {
                Ok(true) => {}
                Ok(false) => {
                    self.remotes.remove(&session);
                }
                Err(failure) if failure.code == "engine_error" => {
                    self.remotes.remove(&session);
                }
                Err(failure) => return Err(failure),
            }
        }
        // A page a session's tab opened is that session's tab.
        for (target, opener) in std::mem::take(&mut self.unassigned) {
            let owner = opener
                .as_deref()
                .and_then(|opener| self.owner_of_target(opener));
            let Some(key) = owner else {
                self.unassigned.push((target, opener));
                continue;
            };
            let opener_tab = self.shares[&key]
                .tabs
                .iter()
                .find(|tab| Some(&tab.target) == opener.as_ref())
                .map(|tab| tab.id.clone());
            // A tab that closed as fast as it opened is no tab.
            let id = match self.adopt(&key, &target, opener_tab.clone()) {
                Ok(id) => id,
                Err(failure) if failure.code == "engine_error" => continue,
                Err(failure) => return Err(failure),
            };
            let url = match self.target_info(&target) {
                Ok((url, _)) => url,
                Err(failure) if failure.code == "engine_error" => String::new(),
                Err(failure) => return Err(failure),
            };
            if let Some(share) = self.shares.get_mut(&key) {
                share.pending.push(Event::NewTab {
                    tab: id,
                    url,
                    opener: opener_tab,
                });
            }
        }
        Ok(events)
    }

    fn target_info(&mut self, target: &str) -> Result<(String, String), Failure> {
        let info = self.cdp.call(
            "Target.getTargetInfo",
            json!({"targetId": target}),
            None,
            Instant::now() + COMMAND,
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
        self.cdp
            .reply_or(id, method, Instant::now() + COMMAND, opened)?;
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
        let tree = self.frame_command(
            "Page.getFrameTree",
            json!({}),
            session,
            Instant::now() + COMMAND,
        )?;
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
    ) -> Result<Value, Failure> {
        let Some(placement) = self.placements.get(&frame.id).cloned() else {
            return Err(stale());
        };
        let key = (frame.id.clone(), frame.document.clone());
        let deadline = Instant::now() + COMMAND;
        for attempt in 0..2 {
            let context = match self.worlds.get(&key) {
                Some(context) => *context,
                None => {
                    let world = self.frame_command(
                        "Page.createIsolatedWorld",
                        json!({"frameId": frame.id, "worldName": "latch", "grantUniveralAccess": false}),
                        &placement.session,
                        deadline,
                    ).map_err(|_| stale())?;
                    let context = world["executionContextId"].as_i64().ok_or_else(stale)?;
                    self.worlds.insert(key.clone(), context);
                    context
                }
            };
            let here = self.tab_sessions();
            let opened = |event: &Value| dialog_in(event, &here);
            match self.session_call(
                "Runtime.evaluate",
                json!({
                    "expression": format!("{READER}\n{expression}"),
                    "contextId": context,
                    "returnByValue": by_value,
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
                        return Err(Failure::failed(
                            "engine_error",
                            format!("latch's page script: {}", clip(text, 200)),
                        ));
                    }
                    return Ok(reply["result"].clone());
                }
                Ok(Reply::Stopped) => {
                    return Err(Failure::failed(
                        "dialog_open",
                        "a dialog is open; answer it with `dialog accept|dismiss`".into(),
                    ));
                }
                Ok(Reply::Gone) => return Err(stale()),
                // The document went away under its world: make a new one once.
                Err(failure) if attempt == 0 && failure.code == "engine_error" => {
                    self.worlds.remove(&key);
                }
                Err(failure) => return Err(failure),
            }
        }
        Err(stale())
    }

    /// Loads `url` in the active tab and waits for its load event.
    fn load(&mut self, url: &str) -> Result<(), Failure> {
        let deadline = Instant::now() + COMMAND;
        let session = self.active_state().session.clone();
        self.absorb()?;
        let navigated = self.cdp.call(
            "Page.navigate",
            json!({"url": url}),
            Some(&session),
            deadline,
        )?;
        if let Some(error) = navigated["errorText"].as_str() {
            return Err(Failure::failed("navigation_failed", error.to_string()));
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
                return Err(Failure::failed(
                    "timeout",
                    format!("{url} did not finish loading"),
                ));
            }
        }
    }
}

impl Driver for Cft {
    fn tabs(&mut self) -> Result<Vec<Tab>, Failure> {
        self.cdp.pump(Duration::ZERO)?;
        self.absorb()?;
        let ids: Vec<(String, String, Option<String>)> = self
            .share()
            .tabs
            .iter()
            .map(|tab| (tab.id.clone(), tab.target.clone(), tab.opener.clone()))
            .collect();
        let mut tabs = Vec::new();
        for (tab, target, opener) in ids {
            let (url, title) = self.target_info(&target)?;
            tabs.push(Tab {
                active: tab == self.share().active,
                tab,
                url,
                title,
                opener,
            });
        }
        Ok(tabs)
    }

    fn new_tab(&mut self, url: Option<&str>) -> Result<String, Failure> {
        let target = self.new_window(Instant::now() + COMMAND)?;
        let key = self.current.clone();
        let tab = self.adopt(&key, &target, None)?;
        self.share_mut().active = tab.clone();
        if let Some(url) = url {
            self.load(url)?;
        }
        Ok(tab)
    }

    /// Makes `tab` the one the agent acts on. With the pinned focus
    /// `background` nothing comes to the front (`Target.activateTarget`
    /// would take the owner's focus): a tab latch opened is its own window,
    /// and a tab a page opened is the visible one in its opener's window.
    fn switch_tab(&mut self, tab: &str) -> Result<(), Failure> {
        let target = self.tab(tab)?.target.clone();
        if CFT.focus == Focus::Front {
            self.cdp.call(
                "Target.activateTarget",
                json!({"targetId": target}),
                None,
                Instant::now() + COMMAND,
            )?;
        }
        self.share_mut().active = tab.to_string();
        Ok(())
    }

    fn close_tab(&mut self, tab: &str) -> Result<(), Failure> {
        let target = self.tab(tab)?.target.clone();
        if self.share().tabs.len() == 1 {
            return Err(Failure::usage(
                "last_tab",
                "a session keeps at least one tab; close the session instead".into(),
            ));
        }
        self.cdp.call(
            "Target.closeTarget",
            json!({"targetId": target}),
            None,
            Instant::now() + COMMAND,
        )?;
        let share = self.share_mut();
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
        self.share().active.clone()
    }

    fn navigate(&mut self, url: &str) -> Result<(), Failure> {
        self.load(url)
    }

    fn back(&mut self) -> Result<bool, Failure> {
        let session = self.active_state().session.clone();
        let deadline = Instant::now() + COMMAND;
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

    fn back_url(&mut self) -> Result<Option<String>, Failure> {
        let session = self.active_state().session.clone();
        let history = self.cdp.call(
            "Page.getNavigationHistory",
            json!({}),
            Some(&session),
            Instant::now() + COMMAND,
        )?;
        let current = history["currentIndex"].as_u64().unwrap_or(0) as usize;
        Ok(current
            .checked_sub(1)
            .and_then(|at| history["entries"][at]["url"].as_str())
            .map(str::to_string))
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
        self.evaluate(frame, expression, true)
            .map(|object| object["value"].clone())
    }

    fn call(&mut self, frame: &Frame, function: &str, args: Vec<Value>) -> Result<Value, Failure> {
        // The reader is defined, and its world made, by an ordinary run.
        self.evaluate(frame, "true", true)?;
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
            Instant::now() + COMMAND,
            opened,
        ) {
            Ok(Reply::Answer(reply)) if reply.get("exceptionDetails").is_some() => Err(
                Failure::failed("engine_error", "latch's page script failed".into()),
            ),
            Ok(Reply::Answer(reply)) => Ok(reply["result"]["value"].clone()),
            Ok(Reply::Stopped) => Err(Failure::failed(
                "dialog_open",
                "a dialog is open; answer it with `dialog accept|dismiss`".into(),
            )),
            Ok(Reply::Gone) | Err(_) => Err(stale()),
        }
    }

    fn reaches(&mut self, frame: &Frame, x: f64, y: f64) -> Result<bool, Failure> {
        let deadline = Instant::now() + COMMAND;
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
            // The point in the parent's viewport must land on this frame's
            // own <iframe>, not on something of the parent's drawn over it.
            let (dx, dy) = self.frame_offset(&parent_frame)?;
            let hit = self.evaluate(
                &parent_frame,
                &format!("document.elementFromPoint({}, {})", x - dx, y - dy),
                false,
            )?;
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

    fn frame_offset(&mut self, frame: &Frame) -> Result<(f64, f64), Failure> {
        let mut offset = (0.0, 0.0);
        let mut current = frame.clone();
        let deadline = Instant::now() + COMMAND;
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
        let (from_x, from_y) = self.share().mouse;
        for step in 1..=4 {
            let t = f64::from(step) / 4.0;
            self.input(
                "Input.dispatchMouseEvent",
                json!({"type": "mouseMoved", "x": from_x + (x - from_x) * t, "y": from_y + (y - from_y) * t}),
            )?;
            pause(8, 16);
        }
        self.share_mut().mouse = (x, y);
        let press =
            |kind: &str| json!({"type": kind, "x": x, "y": y, "button": "left", "clickCount": 1});
        self.input("Input.dispatchMouseEvent", press("mousePressed"))?;
        pause(40, 90);
        self.input("Input.dispatchMouseEvent", press("mouseReleased"))
    }

    fn type_text(&mut self, text: &str) -> Result<(), Failure> {
        for ch in text.chars() {
            if self.share().dialog.is_some() {
                break;
            }
            if ch == '\n' {
                self.press("Enter")?;
            } else {
                let text = ch.to_string();
                let key = Key {
                    key: &text,
                    code: "",
                    key_code: 0,
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
            Failure::usage(
                "bad_key",
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
        self.cdp.pump(NAVIGATION_STARTS)?;
        let mut events = self.absorb()?;
        let loading = events.iter().any(started) && !events.iter().any(stopped);
        let deadline = Instant::now() + LOADING;
        if loading {
            while self.share().dialog.is_none() && Instant::now() < deadline {
                self.cdp.pump(Duration::from_millis(100))?;
                events = self.absorb()?;
                if events.iter().any(stopped) {
                    break;
                }
            }
        }
        self.cdp.pump(Duration::from_millis(150))?;
        self.absorb()?;
        Ok(std::mem::take(&mut self.share_mut().pending))
    }

    fn dialog_open(&self) -> bool {
        self.share().dialog.is_some()
    }

    fn answer_dialog(&mut self, accept: bool, text: Option<&str>) -> Result<(), Failure> {
        self.cdp.pump(Duration::ZERO)?;
        self.absorb()?;
        let Some(tab) = self.share().dialog.clone() else {
            return Err(Failure::failed("no_dialog", "no dialog is open".into()));
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
            Instant::now() + COMMAND,
        )?;
        self.share_mut().dialog = None;
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
            .reply_or(id, "Runtime.evaluate", Instant::now() + COMMAND, opened)?;
        let Some(reply) = reply else {
            return Err(Failure::failed(
                "dialog_open",
                "the script opened a dialog; answer it with `dialog accept|dismiss`".into(),
            ));
        };
        if let Some(details) = reply.get("exceptionDetails") {
            let text = details["exception"]["description"]
                .as_str()
                .or(details["text"].as_str())
                .unwrap_or("the script threw");
            return Err(Failure::failed("eval_failed", clip(text, 1000)));
        }
        Ok(reply["result"]["value"].clone())
    }

    fn set_files(
        &mut self,
        frame: &Frame,
        element: &str,
        files: &[PathBuf],
    ) -> Result<(), Failure> {
        let object = self.evaluate(frame, element, false)?;
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
            Instant::now() + COMMAND,
        )?;
        Ok(())
    }

    fn upload_folder(&self) -> PathBuf {
        self.share().folder.join("uploads")
    }

    fn downloads(&mut self) -> Result<Vec<Download>, Failure> {
        self.cdp.pump(Duration::ZERO)?;
        self.absorb()?;
        Ok(self
            .share()
            .downloads
            .iter()
            .map(|(_, d)| d.clone())
            .collect())
    }

    fn download_file(&mut self, id: &str) -> Result<(Download, PathBuf), Failure> {
        let deadline = Instant::now() + LOADING;
        loop {
            self.cdp.pump(Duration::from_millis(100))?;
            self.absorb()?;
            let Some((guid, download)) = self
                .share()
                .downloads
                .iter()
                .find(|(_, d)| d.download == id)
            else {
                return Err(Failure::usage(
                    "no_such_download",
                    format!("no download {id}"),
                ));
            };
            match download.state.as_str() {
                "completed" => {
                    return Ok((download.clone(), self.scratch.join("downloads").join(guid)));
                }
                "canceled" => {
                    return Err(Failure::failed(
                        "download_failed",
                        format!("download {id} was canceled"),
                    ));
                }
                _ if Instant::now() >= deadline => {
                    return Err(Failure::failed(
                        "download_incomplete",
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
        let deadline = Instant::now() + COMMAND;
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
            .ok_or_else(|| Failure::failed("engine_error", "no screenshot".into()))?;
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
        while unsafe { libc::killpg(group, 0) } == 0 && Instant::now() < gone_by {
            std::thread::sleep(Duration::from_millis(50));
        }
        let left = unsafe { libc::killpg(group, 0) } == 0;
        json!({"closed": !left, "forced": forced})
    }
}

/// One session's view of a shared browser: the driver contract on that
/// session's own tabs, downloads and dialogs. The worker keeps one per
/// session, all on the one browser, and makes one call at a time.
pub struct Seat {
    browser: Rc<RefCell<Cft>>,
    key: String,
}

impl Seat {
    /// Starts session `key` in `browser`, with a window of its own.
    pub fn begin(browser: &Rc<RefCell<Cft>>, key: &str) -> Result<Seat, Failure> {
        browser.borrow_mut().begin(key)?;
        Ok(Seat {
            browser: browser.clone(),
            key: key.to_string(),
        })
    }

    fn cft(&self) -> RefMut<'_, Cft> {
        let mut cft = self.browser.borrow_mut();
        cft.select(&self.key);
        cft
    }
}

impl Driver for Seat {
    fn tabs(&mut self) -> Result<Vec<Tab>, Failure> {
        self.cft().tabs()
    }
    fn new_tab(&mut self, url: Option<&str>) -> Result<String, Failure> {
        self.cft().new_tab(url)
    }
    fn switch_tab(&mut self, tab: &str) -> Result<(), Failure> {
        self.cft().switch_tab(tab)
    }
    fn close_tab(&mut self, tab: &str) -> Result<(), Failure> {
        self.cft().close_tab(tab)
    }
    fn active_tab(&self) -> String {
        self.cft().active_tab()
    }
    fn navigate(&mut self, url: &str) -> Result<(), Failure> {
        self.cft().navigate(url)
    }
    fn back(&mut self) -> Result<bool, Failure> {
        self.cft().back()
    }
    fn back_url(&mut self) -> Result<Option<String>, Failure> {
        self.cft().back_url()
    }
    fn frames(&mut self) -> Result<Vec<Frame>, Failure> {
        self.cft().frames()
    }
    fn run(&mut self, frame: &Frame, expression: &str) -> Result<Value, Failure> {
        self.cft().run(frame, expression)
    }
    fn call(&mut self, frame: &Frame, function: &str, args: Vec<Value>) -> Result<Value, Failure> {
        self.cft().call(frame, function, args)
    }
    fn reaches(&mut self, frame: &Frame, x: f64, y: f64) -> Result<bool, Failure> {
        self.cft().reaches(frame, x, y)
    }
    fn frame_offset(&mut self, frame: &Frame) -> Result<(f64, f64), Failure> {
        self.cft().frame_offset(frame)
    }
    fn click(&mut self, x: f64, y: f64) -> Result<(), Failure> {
        self.cft().click(x, y)
    }
    fn type_text(&mut self, text: &str) -> Result<(), Failure> {
        self.cft().type_text(text)
    }
    fn select_all(&mut self) -> Result<(), Failure> {
        self.cft().select_all()
    }
    fn press(&mut self, key: &str) -> Result<(), Failure> {
        self.cft().press(key)
    }
    fn wheel(&mut self, x: f64, y: f64, delta_y: f64) -> Result<(), Failure> {
        self.cft().wheel(x, y, delta_y)
    }
    fn settle(&mut self) -> Result<Vec<Event>, Failure> {
        self.cft().settle()
    }
    fn dialog_open(&self) -> bool {
        self.cft().dialog_open()
    }
    fn answer_dialog(&mut self, accept: bool, text: Option<&str>) -> Result<(), Failure> {
        self.cft().answer_dialog(accept, text)
    }
    fn eval(&mut self, script: &str) -> Result<Value, Failure> {
        self.cft().eval(script)
    }
    fn set_files(
        &mut self,
        frame: &Frame,
        element: &str,
        files: &[PathBuf],
    ) -> Result<(), Failure> {
        self.cft().set_files(frame, element, files)
    }
    fn upload_folder(&self) -> PathBuf {
        self.cft().upload_folder()
    }
    fn downloads(&mut self) -> Result<Vec<Download>, Failure> {
        self.cft().downloads()
    }
    fn download_file(&mut self, id: &str) -> Result<(Download, PathBuf), Failure> {
        self.cft().download_file(id)
    }
    fn capabilities(&self) -> Vec<String> {
        self.cft().capabilities()
    }
    fn screenshot(&mut self) -> Result<(Value, Vec<u8>), Failure> {
        self.cft().screenshot()
    }
    /// Ends this session only: its tabs close, the browser and the other
    /// sessions go on.
    fn close(&mut self) -> Value {
        self.browser.borrow_mut().end(&self.key);
        json!({"closed": true})
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

const KEY_NAMES: &[&str] = &[
    "Enter",
    "Tab",
    "Escape",
    "Backspace",
    "Delete",
    "Space",
    "ArrowUp",
    "ArrowDown",
    "ArrowLeft",
    "ArrowRight",
    "Home",
    "End",
    "PageUp",
    "PageDown",
];

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
        Exit::Failed,
        "stale_snapshot",
        "that element's document was replaced; observe again".into(),
    )
}

/// Starts the engine headed, on its profile, with the CDP pipe as its fd 3
/// (commands in) and fd 4 (replies out), in a process group of its own.
fn start(
    engine: &Engine,
    data: &Path,
    scratch: &Path,
    reads: &impl AsRawFd,
    writes: &impl AsRawFd,
) -> Result<Child, Failure> {
    for folder in ["tmp", "crash"] {
        fs::create_dir_all(scratch.join(folder)).map_err(io_failure)?;
    }
    let log = File::create(scratch.join("engine.log")).map_err(io_failure)?;
    let (reads, writes) = (reads.as_raw_fd(), writes.as_raw_fd());
    let mut command = Command::new(&engine.executable);
    command
        .args(&engine.recipe)
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
    command.spawn().map_err(|err| {
        Failure::failed(
            "engine_failed",
            format!("{}: {err}", engine.executable.display()),
        )
    })
}

pub fn io_failure(err: io::Error) -> Failure {
    Failure::failed("engine_failed", err.to_string())
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

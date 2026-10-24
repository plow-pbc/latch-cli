//! The driver contract: what the shared core (`core.rs`) needs from an
//! engine. Each engine family implements it once (`cft.rs` for Chrome for
//! Testing, `camoufox.rs` for Camoufox); everything above it, the page
//! reader, snapshots and refs,
//! actions and results, is written once for all of them.
//!
//! All coordinates are CSS pixels in the active tab's viewport. Input is
//! the engine's own ("trusted") mouse and keyboard, never a script's
//! `element.click()`.

use std::path::PathBuf;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::failure::Failure;

/// The version of these operations; a plugin's manifest names the one its
/// driver speaks.
pub const CONTRACT: u32 = 1;

#[derive(Clone, Serialize, Deserialize)]
pub struct Tab {
    pub tab: String,
    pub url: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opener: Option<String>,
    pub active: bool,
}

/// One frame of the active tab, in tree order (the main frame first).
#[derive(Clone)]
pub struct Frame {
    /// The engine's own frame id.
    pub id: String,
    pub parent: Option<String>,
    /// The frame's current document; a new one replaces every ref in it.
    pub document: String,
}

/// A download the session's browser started, as a handle; its file stays on
/// the host until `download get` brings it to the client.
#[derive(Clone, Serialize, Deserialize)]
pub struct Download {
    pub download: String,
    /// The name the site suggested.
    pub name: String,
    pub url: String,
    /// `in_progress`, `completed` or `canceled`. Only a completed one counts.
    pub state: String,
    pub bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
}

/// Something the page did that the agent should hear about.
#[derive(Clone, Serialize, Deserialize, PartialEq, Debug)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Dialog {
        tab: String,
        kind: String,
        message: String,
    },
    NewTab {
        tab: String,
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        opener: Option<String>,
    },
    TabClosed {
        tab: String,
    },
    Download {
        download: String,
        name: String,
        state: String,
    },
    /// A page's own request (a page load, an XHR or a fetch) that the site
    /// answered with an error status (a 429 to a sign-in, say), by the
    /// site's origin only: never its path or query.
    HttpError {
        tab: String,
        status: u16,
        origin: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry_after: Option<String>,
    },
}

/// The most `HttpError` events kept between two answers.
pub const HTTP_ERRORS: usize = 5;

/// The keys `press` takes, by name.
pub const KEY_NAMES: &[&str] = &[
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

pub trait Driver {
    /// Bounds the call under way: until it is lifted (`None`), no wait for
    /// the engine lasts past `deadline`, however many commands the call
    /// sends. Each command still has its own, shorter, limit.
    fn limit(&mut self, deadline: Option<Instant>);

    fn tabs(&mut self) -> Result<Vec<Tab>, Failure>;
    /// Opens a tab (on `url`, or blank) and makes it the active one.
    fn new_tab(&mut self, url: Option<&str>) -> Result<String, Failure>;
    fn switch_tab(&mut self, tab: &str) -> Result<(), Failure>;
    /// Brings the session's active tab, its window and the browser to the
    /// front, for the owner: what only a handoff does.
    fn front(&mut self) -> Result<(), Failure>;
    fn close_tab(&mut self, tab: &str) -> Result<(), Failure>;
    fn active_tab(&self) -> String;

    /// Loads `url` in the active tab and waits for it.
    fn navigate(&mut self, url: &str) -> Result<(), Failure>;
    /// One step back in the active tab's history; false if there is none.
    fn back(&mut self) -> Result<bool, Failure>;

    /// Every frame of the active tab, cross-site and sandboxed ones included.
    fn frames(&mut self) -> Result<Vec<Frame>, Failure>;
    /// Runs latch's script in an isolated world of `frame` (the page's own
    /// scripts never see it) and returns its JSON value.
    fn run(&mut self, frame: &Frame, expression: &str) -> Result<Value, Failure>;
    /// Calls `function` (a function expression that may use latch's
    /// reader) in `frame`'s isolated world with `args`, passed as call
    /// arguments rather than as script text: how a vault value is handed
    /// to the page.
    fn call(&mut self, frame: &Frame, function: &str, args: Vec<Value>) -> Result<Value, Failure>;
    /// Whether a click at (`x`, `y`) in the tab's viewport reaches `frame`
    /// through every ancestor: each parent document has that frame's own
    /// element there, not something of its own drawn over it.
    fn reaches(&mut self, frame: &Frame, x: f64, y: f64) -> Result<bool, Failure>;
    /// Whether `frame` holds the page's focus, so the engine's keys go
    /// into it: each parent document's active element is that frame's own
    /// element.
    fn holds_focus(&mut self, frame: &Frame) -> Result<bool, Failure>;
    /// Where `frame`'s viewport starts in the tab's viewport.
    fn frame_offset(&mut self, frame: &Frame) -> Result<(f64, f64), Failure>;

    fn click(&mut self, x: f64, y: f64) -> Result<(), Failure>;
    /// Types `text` key by key, at a person's pace, into the focused element.
    fn type_text(&mut self, text: &str) -> Result<(), Failure>;
    /// Selects everything in the focused field.
    fn select_all(&mut self) -> Result<(), Failure>;
    /// One named key: `Enter`, `Tab`, `Escape`, `ArrowDown`, …
    fn press(&mut self, key: &str) -> Result<(), Failure>;
    fn wheel(&mut self, x: f64, y: f64, delta_y: f64) -> Result<(), Failure>;

    /// Waits for what an input started (a navigation, a load) to finish,
    /// or a dialog to open, and returns the events since the last call.
    fn settle(&mut self) -> Result<Vec<Event>, Failure>;
    /// The open dialog, if one blocks the active tab.
    fn dialog_open(&self) -> bool;
    fn answer_dialog(&mut self, accept: bool, text: Option<&str>) -> Result<(), Failure>;

    /// Runs the agent's `script` in the active tab's main frame, in the
    /// page's own world, and returns its JSON value (a promise is awaited).
    fn eval(&mut self, script: &str) -> Result<Value, Failure>;
    /// Puts `files` (paths on the host) into the file input that
    /// `element`, a reader expression, finds in `frame`.
    fn set_files(&mut self, frame: &Frame, element: &str, files: &[PathBuf])
    -> Result<(), Failure>;
    /// The folder uploads are staged in for this session.
    fn upload_folder(&self) -> PathBuf;
    fn downloads(&mut self) -> Result<Vec<Download>, Failure>;
    /// The file of completed download `id`, waiting a while for one still
    /// in progress.
    fn download_file(&mut self, id: &str) -> Result<(Download, PathBuf), Failure>;

    /// Optional features this engine has (`eval`, `upload`, `downloads`);
    /// a command needing another is refused with `unsupported_capability`
    /// before anything is dispatched.
    fn capabilities(&self) -> Vec<String>;

    /// A PNG of the active tab's viewport and how its pixels map to CSS pixels.
    fn screenshot(&mut self) -> Result<(Value, Vec<u8>), Failure>;

    /// Until called again with none, the browser stops every GET request,
    /// in any tab, whose address has a query parameter equal to one of
    /// `values`: a form sent by GET would put a filled vault value there.
    fn guard(&mut self, values: &[String]) -> Result<(), Failure> {
        let _ = values;
        Ok(())
    }
    /// Whether the guard stopped a request since this was last asked.
    fn guard_stopped(&mut self) -> bool {
        false
    }
    /// Closes the browser and reaps every process it started.
    fn close(&mut self) -> Value;
}

/// A session's browser, as the worker drives it between calls.
pub trait Runtime: Driver {
    /// Whether the engine left a command unanswered in time since this
    /// was last asked.
    fn unanswered(&mut self) -> bool;
    /// Between calls: reads what the engine sent that cannot wait for the
    /// next call. Answers whether it must be called again soon.
    fn tend(&mut self) -> bool {
        false
    }
}

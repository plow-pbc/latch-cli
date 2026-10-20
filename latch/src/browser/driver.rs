//! The driver contract: what the shared core (`core.rs`) needs from an
//! engine. Each engine family implements it once (`cft.rs` for Chrome for
//! Testing, `camoufox.rs` for Camoufox); everything above it, the page
//! reader, snapshots and refs,
//! actions and results, is written once for all of them.
//!
//! All coordinates are CSS pixels in the active tab's viewport. Input is
//! the engine's own ("trusted") mouse and keyboard, never a script's
//! `element.click()`.

use std::cell::{RefCell, RefMut};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

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
}

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
    fn close_tab(&mut self, tab: &str) -> Result<(), Failure>;
    fn active_tab(&self) -> String;

    /// Loads `url` in the active tab and waits for it.
    fn navigate(&mut self, url: &str) -> Result<(), Failure>;
    /// One step back in the active tab's history; false if there is none.
    fn back(&mut self) -> Result<bool, Failure>;
    /// The address `back` would load, if there is one; an error when the
    /// engine can't tell (Camoufox, when that entry is another origin's).
    fn back_url(&mut self) -> Result<Option<String>, Failure>;

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
    /// Closes the browser and reaps every process it started.
    fn close(&mut self) -> Value;
}

/// One profile's browser, shared by the sessions on it: the driver
/// contract acts for the selected session, on its own tabs, downloads and
/// dialogs.
pub trait Runtime: Driver {
    /// Starts session `key` with a window of its own, and selects it.
    fn begin(&mut self, key: &str) -> Result<(), Failure>;
    /// Ends session `key`: closes every tab it has, and forgets its
    /// downloads (their files too). The others go on.
    fn end(&mut self, key: &str);
    /// Makes session `key` the one the driver contract acts for.
    fn select(&mut self, key: &str);
}

/// One session's view of a shared browser: the driver contract on that
/// session's own tabs, downloads and dialogs. The worker keeps one per
/// session, all on the one browser, and makes one call at a time.
pub struct Seat<B> {
    browser: Rc<RefCell<B>>,
    key: String,
}

impl<B: Runtime> Seat<B> {
    /// Starts session `key` in `browser`, with a window of its own.
    pub fn begin(browser: &Rc<RefCell<B>>, key: &str) -> Result<Seat<B>, Failure> {
        browser.borrow_mut().begin(key)?;
        Ok(Seat {
            browser: browser.clone(),
            key: key.to_string(),
        })
    }

    fn browser(&self) -> RefMut<'_, B> {
        let mut browser = self.browser.borrow_mut();
        browser.select(&self.key);
        browser
    }
}

impl<B: Runtime> Driver for Seat<B> {
    fn limit(&mut self, deadline: Option<Instant>) {
        self.browser.borrow_mut().limit(deadline);
    }
    fn tabs(&mut self) -> Result<Vec<Tab>, Failure> {
        self.browser().tabs()
    }
    fn new_tab(&mut self, url: Option<&str>) -> Result<String, Failure> {
        self.browser().new_tab(url)
    }
    fn switch_tab(&mut self, tab: &str) -> Result<(), Failure> {
        self.browser().switch_tab(tab)
    }
    fn close_tab(&mut self, tab: &str) -> Result<(), Failure> {
        self.browser().close_tab(tab)
    }
    fn active_tab(&self) -> String {
        self.browser().active_tab()
    }
    fn navigate(&mut self, url: &str) -> Result<(), Failure> {
        self.browser().navigate(url)
    }
    fn back(&mut self) -> Result<bool, Failure> {
        self.browser().back()
    }
    fn back_url(&mut self) -> Result<Option<String>, Failure> {
        self.browser().back_url()
    }
    fn frames(&mut self) -> Result<Vec<Frame>, Failure> {
        self.browser().frames()
    }
    fn run(&mut self, frame: &Frame, expression: &str) -> Result<Value, Failure> {
        self.browser().run(frame, expression)
    }
    fn call(&mut self, frame: &Frame, function: &str, args: Vec<Value>) -> Result<Value, Failure> {
        self.browser().call(frame, function, args)
    }
    fn reaches(&mut self, frame: &Frame, x: f64, y: f64) -> Result<bool, Failure> {
        self.browser().reaches(frame, x, y)
    }
    fn frame_offset(&mut self, frame: &Frame) -> Result<(f64, f64), Failure> {
        self.browser().frame_offset(frame)
    }
    fn click(&mut self, x: f64, y: f64) -> Result<(), Failure> {
        self.browser().click(x, y)
    }
    fn type_text(&mut self, text: &str) -> Result<(), Failure> {
        self.browser().type_text(text)
    }
    fn select_all(&mut self) -> Result<(), Failure> {
        self.browser().select_all()
    }
    fn press(&mut self, key: &str) -> Result<(), Failure> {
        self.browser().press(key)
    }
    fn wheel(&mut self, x: f64, y: f64, delta_y: f64) -> Result<(), Failure> {
        self.browser().wheel(x, y, delta_y)
    }
    fn settle(&mut self) -> Result<Vec<Event>, Failure> {
        self.browser().settle()
    }
    fn dialog_open(&self) -> bool {
        self.browser().dialog_open()
    }
    fn answer_dialog(&mut self, accept: bool, text: Option<&str>) -> Result<(), Failure> {
        self.browser().answer_dialog(accept, text)
    }
    fn eval(&mut self, script: &str) -> Result<Value, Failure> {
        self.browser().eval(script)
    }
    fn set_files(
        &mut self,
        frame: &Frame,
        element: &str,
        files: &[PathBuf],
    ) -> Result<(), Failure> {
        self.browser().set_files(frame, element, files)
    }
    fn upload_folder(&self) -> PathBuf {
        self.browser().upload_folder()
    }
    fn downloads(&mut self) -> Result<Vec<Download>, Failure> {
        self.browser().downloads()
    }
    fn download_file(&mut self, id: &str) -> Result<(Download, PathBuf), Failure> {
        self.browser().download_file(id)
    }
    fn capabilities(&self) -> Vec<String> {
        self.browser().capabilities()
    }
    fn screenshot(&mut self) -> Result<(Value, Vec<u8>), Failure> {
        self.browser().screenshot()
    }
    /// Ends this session only: its tabs close, the browser and the other
    /// sessions go on.
    fn close(&mut self) -> Value {
        self.browser.borrow_mut().end(&self.key);
        json!({"closed": true})
    }
}

//! The Camoufox driver: the driver contract (`driver.rs`) through latch's
//! Camoufox helper (`camoufox/helper.mjs`), a Node program on Playwright
//! that speaks the same operations as JSON lines over its standard streams.
//! Playwright reaches Camoufox over a pipe of its own; no port is opened.
//! The browser is one session's: the helper keeps its tabs, dialogs and
//! downloads.
//!
//! The helper's source is built into latch and written into the session's
//! scratch folder at launch; what it runs on (Node, its packages and
//! Camoufox) is the pinned plugin, checked before every launch.

use std::fs::{self, File};
use std::io::{ErrorKind, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::cdp;
use super::driver::{self, Download, Driver, Event, Frame, KEY_NAMES, Runtime, Tab};
use super::engine::CAMOUFOX;
use super::profile;
use crate::failure::{Failure, code};

/// How long the helper has to start the browser (a launch also makes the
/// session's fingerprint), and to answer one operation: its
/// own deadlines are shorter, so this one only catches a helper that hangs.
const STARTUP: Duration = Duration::from_secs(90);
const COMMAND: Duration = Duration::from_secs(90);
/// How long a closing browser has before its process groups are killed.
const CLOSING: Duration = Duration::from_secs(10);

/// The only variables the helper and the engine get.
const PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

const HELPER: &str = include_str!("../../camoufox/helper.mjs");
const READER: &str = include_str!("reader.js");

pub struct Camoufox {
    helper: Child,
    to_helper: ChildStdin,
    from_helper: ChildStdout,
    buffer: Vec<u8>,
    next_id: u64,
    /// The browser's own process, which Playwright starts in a process
    /// group of its own.
    browser: u32,
    /// With no window: nothing to bring to the front.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    headless: bool,
    /// When the call under way must end (`Driver::limit`).
    deadline: Option<Instant>,
    /// An answer did not come in time since this was last asked.
    unanswered: bool,
    /// What the helper said with its latest answer: whether a dialog
    /// blocks the session, and its active tab.
    seen: (bool, String),
    scratch: PathBuf,
}

impl Camoufox {
    /// Starts the helper and, through it, the engine on the session's
    /// profile in folder `profile`; with `headless`, with no window.
    pub fn launch(
        state_dir: &Path,
        profile: &Path,
        scratch: &Path,
        headless: bool,
        display: Option<&str>,
        proxy: &str,
    ) -> Result<Self, Failure> {
        if CAMOUFOX.driver_contract != driver::CONTRACT {
            return Err(Failure::new(
                code::engine_unverified,
                format!(
                    "the {} plugin speaks driver contract {}, this latch {}",
                    CAMOUFOX.engine,
                    CAMOUFOX.driver_contract,
                    driver::CONTRACT
                ),
            ));
        }
        let engine = CAMOUFOX.verified(state_dir)?;
        let profile = profile::prepare(profile, &CAMOUFOX.engine)?;
        for folder in ["tmp", "downloads"] {
            fs::create_dir_all(scratch.join(folder)).map_err(io_failure)?;
        }
        let helper_file = scratch.join("helper.mjs");
        fs::write(&helper_file, HELPER).map_err(io_failure)?;
        let log = File::create(scratch.join("engine.log")).map_err(io_failure)?;
        let mut helper = Command::new(&engine.node)
            .arg(&helper_file)
            .arg(&engine.runtime)
            .env_clear()
            .env("PATH", PATH)
            .env("HOME", scratch)
            .env("TMPDIR", scratch.join("tmp"))
            // camoufox-js reads the build from the plugin, and never
            // downloads a build or an add-on itself.
            .env("CAMOUFOX_INSTALL_DIR", &engine.install)
            .env("PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD", "1")
            // Linux fontconfig resolves the bundled fonts relative to cwd.
            .current_dir(if cfg!(target_os = "linux") {
                engine.install.as_path()
            } else {
                scratch
            })
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(log)
            .process_group(0)
            .spawn()
            .map_err(|err| {
                Failure::new(
                    code::engine_failed,
                    format!("{}: {err}", engine.node.display()),
                )
            })?;
        super::worker::started_group(helper.id());
        let to_helper = helper.stdin.take().expect("piped");
        let from_helper = helper.stdout.take().expect("piped");
        let mut browser = Camoufox {
            helper,
            to_helper,
            from_helper,
            buffer: Vec::new(),
            next_id: 0,
            browser: 0,
            headless,
            deadline: None,
            unanswered: false,
            seen: (false, String::new()),
            scratch: scratch.to_path_buf(),
        };
        let mut env = json!({
            "PATH": PATH,
            "HOME": scratch,
            "TMPDIR": scratch.join("tmp"),
            "MOZ_CRASHREPORTER_DISABLE": "1",
        });
        #[cfg(target_os = "linux")]
        {
            // camoufox-js names the fingerprint "mac", but the pinned
            // build's fontconfig folder is named "macos".
            env["FONTCONFIG_PATH"] = json!(
                engine
                    .install
                    .join("fontconfig")
                    .join(&CAMOUFOX.fingerprint_os)
            );
        }
        if let Some(display) = display {
            env["DISPLAY"] = json!(display);
        }
        let launched = browser.request::<Value>(
            "launch",
            json!({
                "data": profile.data(),
                "downloads": scratch.join("downloads"),
                "executable": engine.executable,
                "headless": headless,
                "os": CAMOUFOX.fingerprint_os,
                "reader": READER,
                "env": env,
                "proxy": proxy,
            }),
            STARTUP,
        );
        let found = launched.and_then(|_| {
            child_running(browser.helper.id(), &engine.executable)
                .ok_or_else(|| Failure::new(code::engine_failed, "Camoufox did not start".into()))
        });
        match found {
            Ok(pid) => {
                super::worker::started_group(pid);
                browser.browser = pid;
            }
            Err(failure) => {
                browser.close();
                return Err(failure);
            }
        }
        Ok(browser)
    }

    /// The engine's own process; its process group holds every process of
    /// the browser.
    pub fn pid(&self) -> u32 {
        self.browser
    }

    fn ask<T: DeserializeOwned>(&mut self, op: &str, params: Value) -> Result<T, Failure> {
        self.request(op, params, COMMAND)
    }

    /// One operation, answered by the helper.
    fn request<T: DeserializeOwned>(
        &mut self,
        op: &str,
        mut params: Value,
        wait: Duration,
    ) -> Result<T, Failure> {
        self.next_id += 1;
        let id = self.next_id;
        params["id"] = json!(id);
        params["op"] = json!(op);
        let own = Instant::now() + wait;
        let deadline = self.deadline.map_or(own, |limit| limit.min(own));
        // The helper ends its own waits by then too.
        params["within_ms"] = json!(
            deadline
                .saturating_duration_since(Instant::now())
                .as_millis()
        );
        let mut line = serde_json::to_vec(&params).expect("JSON");
        line.push(b'\n');
        self.to_helper.write_all(&line).map_err(|_| gone())?;
        loop {
            let answer = self.read_line(deadline, op)?;
            // An answer to an operation that timed out earlier is dropped.
            if answer["id"] != id {
                continue;
            }
            // A wait the helper gave up on, though it answered all the same.
            self.unanswered |= answer["late"] == true;
            let active = answer["active"].as_str().unwrap_or_default();
            self.seen = (answer["dialog"] == true, active.to_string());
            if let Some(error) = answer.get("error") {
                let code = error["code"].as_str().unwrap_or("engine_error");
                let message = error["message"].as_str().unwrap_or_default().to_string();
                // The helper's own wait for the browser ran out.
                self.unanswered |= code == "timeout";
                return Err(Failure::reported(code, message));
            }
            return serde_json::from_value(answer["ok"].clone()).map_err(|err| {
                Failure::new(
                    code::engine_error,
                    format!("the helper's {op} answer: {err}"),
                )
            });
        }
    }

    fn read_line(&mut self, deadline: Instant, op: &str) -> Result<Value, Failure> {
        loop {
            if let Some(end) = self.buffer.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = self.buffer.drain(..=end).collect();
                return serde_json::from_slice(&line[..end]).map_err(|_| {
                    Failure::new(
                        code::engine_error,
                        "an unreadable answer from the helper".into(),
                    )
                });
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                self.unanswered = true;
                return Err(Failure::new(
                    code::timeout,
                    format!("the browser did not answer {op} in time"),
                ));
            }
            if !cdp::readable(&self.from_helper, left)? {
                continue;
            }
            let mut chunk = [0u8; 64 * 1024];
            match self.from_helper.read(&mut chunk) {
                Ok(0) => return Err(gone()),
                Ok(n) => self.buffer.extend_from_slice(&chunk[..n]),
                Err(err) if err.kind() == ErrorKind::Interrupted => {}
                Err(_) => return Err(gone()),
            }
        }
    }

    fn frame_params(frame: &Frame) -> Value {
        json!({"frame": frame.id, "document": frame.document})
    }
}

impl Driver for Camoufox {
    fn limit(&mut self, deadline: Option<Instant>) {
        self.deadline = deadline;
    }
    fn guard(&mut self, values: &[String]) -> Result<(), Failure> {
        self.ask::<Value>("guard", json!({"values": values}))?;
        Ok(())
    }
    fn guard_stopped(&mut self) -> bool {
        self.ask::<bool>("guard_stopped", json!({}))
            .unwrap_or(false)
    }

    fn tabs(&mut self) -> Result<Vec<Tab>, Failure> {
        self.ask("tabs", json!({}))
    }

    fn new_tab(&mut self, url: Option<&str>) -> Result<String, Failure> {
        self.ask("new_tab", json!({"url": url}))
    }

    fn switch_tab(&mut self, tab: &str) -> Result<(), Failure> {
        self.ask::<Value>("switch_tab", json!({"tab": tab}))
            .map(drop)
    }

    fn close_tab(&mut self, tab: &str) -> Result<(), Failure> {
        self.ask::<Value>("close_tab", json!({"tab": tab}))
            .map(drop)
    }

    /// The session's tab to the front of its window, then the app to the
    /// front of the owner's screen: Playwright's `bringToFront` only
    /// switches the tab.
    fn front(&mut self) -> Result<(), Failure> {
        self.ask::<Value>("front", json!({}))?;
        #[cfg(target_os = "macos")]
        if !self.headless && !activate(self.browser) {
            crate::log::log(&format!(
                "browser: macOS did not bring Camoufox ({}) to the front for a handoff",
                self.browser
            ));
        }
        Ok(())
    }

    fn active_tab(&self) -> String {
        self.seen.1.clone()
    }

    fn navigate(&mut self, url: &str) -> Result<(), Failure> {
        self.ask::<Value>("navigate", json!({"url": url})).map(drop)
    }

    fn back(&mut self) -> Result<bool, Failure> {
        self.ask("back", json!({}))
    }

    fn frames(&mut self) -> Result<Vec<Frame>, Failure> {
        let listed: Vec<Value> = self.ask("frames", json!({}))?;
        Ok(listed
            .into_iter()
            .map(|frame| Frame {
                id: frame["id"].as_str().unwrap_or_default().to_string(),
                parent: frame["parent"].as_str().map(str::to_string),
                document: frame["document"].as_str().unwrap_or_default().to_string(),
            })
            .collect())
    }

    fn run(&mut self, frame: &Frame, expression: &str) -> Result<Value, Failure> {
        let mut params = Self::frame_params(frame);
        params["expression"] = json!(expression);
        self.ask("run", params)
    }

    fn call(&mut self, frame: &Frame, function: &str, args: Vec<Value>) -> Result<Value, Failure> {
        let mut params = Self::frame_params(frame);
        params["function"] = json!(function);
        params["args"] = json!(args);
        self.ask("call", params)
    }

    fn reaches(&mut self, frame: &Frame, x: f64, y: f64) -> Result<bool, Failure> {
        let mut params = Self::frame_params(frame);
        params["x"] = json!(x);
        params["y"] = json!(y);
        self.ask("reaches", params)
    }

    fn frame_offset(&mut self, frame: &Frame) -> Result<(f64, f64), Failure> {
        self.ask("frame_offset", Self::frame_params(frame))
    }

    fn click(&mut self, x: f64, y: f64) -> Result<(), Failure> {
        self.ask::<Value>("click", json!({"x": x, "y": y}))
            .map(drop)
    }

    fn type_text(&mut self, text: &str) -> Result<(), Failure> {
        self.ask::<Value>("type_text", json!({"text": text}))
            .map(drop)
    }

    fn select_all(&mut self) -> Result<(), Failure> {
        self.ask::<Value>("select_all", json!({})).map(drop)
    }

    fn press(&mut self, key: &str) -> Result<(), Failure> {
        if !KEY_NAMES.contains(&key) {
            return Err(Failure::new(
                code::bad_key,
                format!("unknown key {key:?}; keys: {}", KEY_NAMES.join(", ")),
            ));
        }
        self.ask::<Value>("press", json!({"key": key})).map(drop)
    }

    fn wheel(&mut self, x: f64, y: f64, delta_y: f64) -> Result<(), Failure> {
        self.ask::<Value>("wheel", json!({"x": x, "y": y, "delta_y": delta_y}))
            .map(drop)
    }

    fn settle(&mut self) -> Result<Vec<Event>, Failure> {
        self.ask("settle", json!({}))
    }

    fn dialog_open(&self) -> bool {
        self.seen.0
    }

    fn answer_dialog(&mut self, accept: bool, text: Option<&str>) -> Result<(), Failure> {
        self.ask::<Value>("answer_dialog", json!({"accept": accept, "text": text}))
            .map(drop)
    }

    fn eval(&mut self, script: &str) -> Result<Value, Failure> {
        self.ask("eval", json!({"script": script}))
    }

    fn set_files(
        &mut self,
        frame: &Frame,
        element: &str,
        files: &[PathBuf],
    ) -> Result<(), Failure> {
        let mut params = Self::frame_params(frame);
        params["element"] = json!(element);
        params["files"] = json!(files);
        self.ask::<Value>("set_files", params).map(drop)
    }

    fn upload_folder(&self) -> PathBuf {
        self.scratch.join("uploads")
    }

    fn downloads(&mut self) -> Result<Vec<Download>, Failure> {
        self.ask("downloads", json!({}))
    }

    fn download_file(&mut self, id: &str) -> Result<(Download, PathBuf), Failure> {
        let found: Value = self.ask("download_file", json!({"download": id}))?;
        let download = serde_json::from_value(found["download"].clone())
            .map_err(|err| Failure::new(code::engine_error, err.to_string()))?;
        let path = PathBuf::from(found["path"].as_str().unwrap_or_default());
        Ok((download, path))
    }

    fn capabilities(&self) -> Vec<String> {
        CAMOUFOX.capabilities.clone()
    }

    /// The page as the engine draws it, and how its pixels map to the
    /// page's CSS coordinates: pixel = (css - scroll) * device_pixel_ratio.
    fn screenshot(&mut self) -> Result<(Value, Vec<u8>), Failure> {
        let shot: Value = self.ask("screenshot", json!({}))?;
        let png = shot["png"]
            .as_str()
            .and_then(cdp::base64_decode)
            .filter(|png| png.starts_with(b"\x89PNG\r\n\x1a\n") && png.len() >= 24)
            .ok_or_else(|| Failure::new(code::engine_error, "no screenshot".into()))?;
        let width = u32::from_be_bytes(png[16..20].try_into().expect("4 bytes"));
        let height = u32::from_be_bytes(png[20..24].try_into().expect("4 bytes"));
        let css_width = shot["css_width"].as_f64().unwrap_or(0.0);
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
                "css_height": shot["css_height"],
                "scroll_x": shot["scroll_x"],
                "scroll_y": shot["scroll_y"],
                "device_pixel_ratio": ratio,
            },
        });
        Ok((meta, png))
    }

    /// Asks the helper to close the browser, waits for it, then kills
    /// whatever is left of the helper's and the browser's process groups
    /// and waits until both are empty.
    /// While a session holds a guard, the browser is killed at once, as
    /// closing its pages would let them send what they hold.
    fn close(&mut self) -> Value {
        let guarded = self.ask::<bool>("guarded", json!({})).unwrap_or(true);
        let mut forced = guarded;
        if !guarded {
            let deadline = Instant::now() + CLOSING;
            let _ = self.request::<Value>("close", json!({}), CLOSING);
            while self.helper.try_wait().ok().flatten().is_none() {
                if Instant::now() >= deadline {
                    forced = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        let closed = self.kill();
        json!({"closed": closed, "forced": forced})
    }
}

impl Camoufox {
    /// Kills the helper and the browser, with every process each started,
    /// and waits a while for them to go; whether they did.
    fn kill(&mut self) -> bool {
        let groups: Vec<libc::pid_t> = [self.helper.id(), self.browser]
            .into_iter()
            .filter(|pid| *pid != 0)
            .map(|pid| pid as libc::pid_t)
            .collect();
        for group in &groups {
            // SAFETY: signals only; the helper and the browser each lead
            // their own process group.
            unsafe { libc::killpg(*group, libc::SIGKILL) };
        }
        let _ = self.helper.wait();
        let gone_by = Instant::now() + CLOSING;
        // A group with no process left answers ESRCH.
        // SAFETY: signal 0 only checks whether the group still exists.
        let left = || groups.iter().any(|g| unsafe { libc::killpg(*g, 0) } == 0);
        while left() && Instant::now() < gone_by {
            std::thread::sleep(Duration::from_millis(50));
        }
        !left()
    }
}

impl Runtime for Camoufox {
    fn unanswered(&mut self) -> bool {
        std::mem::take(&mut self.unanswered)
    }
}

/// The running child of `parent` that runs `executable`.
fn child_running(parent: u32, executable: &Path) -> Option<u32> {
    let out = Command::new("/bin/ps")
        .args(["-axww", "-o", "pid=,ppid=,args="])
        .output()
        .ok()?;
    let executable = executable.to_string_lossy();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let ppid: u32 = fields.next()?.parse().ok()?;
            let args = fields.collect::<Vec<_>>().join(" ");
            (ppid == parent && args.starts_with(executable.as_ref())).then_some(pid)
        })
}

fn gone() -> Failure {
    Failure::new(code::browser_gone, "the browser stopped".into())
}

fn io_failure(err: std::io::Error) -> Failure {
    Failure::new(code::engine_failed, err.to_string())
}

/// Makes `pid`'s app the front one; false if it is no app or macOS
/// declined.
#[cfg(target_os = "macos")]
fn activate(pid: u32) -> bool {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};

    // `NSRunningApplication` lives in AppKit.
    #[link(name = "AppKit", kind = "framework")]
    unsafe extern "C" {}

    objc2::rc::autoreleasepool(|_| {
        let Some(class) = AnyClass::get(c"NSRunningApplication") else {
            return false;
        };
        // SAFETY: a class method that returns an autoreleased
        // NSRunningApplication or nil.
        let app: *mut AnyObject =
            unsafe { msg_send![class, runningApplicationWithProcessIdentifier: pid as i32] };
        // SAFETY: `app` is a live NSRunningApplication; options 0 brings its
        // frontmost window.
        !app.is_null() && unsafe { msg_send![app, activateWithOptions: 0usize] }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn a_process_that_is_no_app_is_not_brought_forward() {
        assert!(!activate(std::process::id()));
    }

    /// A Camoufox whose helper is `script`, run by `sh`.
    fn with_helper(state: &Path, script: &str) -> Camoufox {
        let mut helper = Command::new("/bin/sh")
            .args(["-c", script])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        Camoufox {
            to_helper: helper.stdin.take().unwrap(),
            from_helper: helper.stdout.take().unwrap(),
            helper,
            buffer: Vec::new(),
            next_id: 0,
            browser: 0,
            headless: true,
            deadline: None,
            unanswered: false,
            seen: (false, String::new()),
            scratch: state.to_path_buf(),
        }
    }

    #[test]
    fn a_timeout_the_helper_reports_is_a_command_left_unanswered() {
        let state = tempfile::tempdir().unwrap();
        let mut browser = with_helper(
            state.path(),
            r#"read line; echo '{"id": 1, "error": {"code": "timeout", "message": "the browser did not answer in time"}}'; read line; echo '{"id": 2, "error": {"code": "stale_snapshot", "message": "gone"}}'"#,
        );
        let failure = browser.ask::<Value>("tabs", json!({})).err().unwrap();
        assert_eq!(failure.code, "timeout");
        assert!(browser.unanswered());
        // Asked once; and an answered error is no unanswered command.
        assert!(!browser.unanswered());
        browser.ask::<Value>("tabs", json!({})).err().unwrap();
        assert!(!browser.unanswered());
        let _ = browser.helper.kill();
    }
}

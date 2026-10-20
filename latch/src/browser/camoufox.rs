//! The Camoufox driver: the driver contract (`driver.rs`) through latch's
//! Camoufox helper (`camoufox/helper.mjs`), a Node program on Playwright
//! that speaks the same operations as JSON lines over its standard streams.
//! Playwright reaches Camoufox over a pipe of its own; no port is opened.
//! One browser serves every session on its profile; the helper keeps each
//! session's tabs, dialogs and downloads apart, and every operation names
//! the session it acts for.
//!
//! The helper's source is built into latch and written into the session's
//! scratch folder at launch; what it runs on (Node, its packages and
//! Camoufox) is the pinned plugin, checked before every launch.

use std::collections::HashMap;
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
use crate::failure::{Exit, Failure};

/// How long the helper has to start the browser (the first launch of a
/// profile also makes its fingerprint), and to answer one operation: its
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
    /// The session the driver contract acts for.
    current: String,
    /// When the call under way must end (`Driver::limit`).
    deadline: Option<Instant>,
    /// An answer did not come in time since this was last asked.
    unanswered: bool,
    /// What the helper said about each session with its latest answer:
    /// whether a dialog blocks it, and its active tab.
    seen: HashMap<String, (bool, String)>,
    scratch: PathBuf,
    _lock: profile::Lock,
}

impl Camoufox {
    /// Starts the helper and, through it, the engine on its profile; with
    /// `headless`, with no window.
    pub fn launch(
        state_dir: &Path,
        profile_name: &str,
        scratch: &Path,
        headless: bool,
        display: Option<&str>,
    ) -> Result<Self, Failure> {
        if CAMOUFOX.driver_contract != driver::CONTRACT {
            return Err(Failure::failed(
                "engine_unverified",
                format!(
                    "the {} plugin speaks driver contract {}, this latch {}",
                    CAMOUFOX.engine,
                    CAMOUFOX.driver_contract,
                    driver::CONTRACT
                ),
            ));
        }
        let profile = profile::open(state_dir, profile_name, &CAMOUFOX.engine)?;
        let lock = profile::lock(&profile)?;
        let engine = CAMOUFOX.verified(state_dir)?;
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
            .current_dir(scratch)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(log)
            .process_group(0)
            .spawn()
            .map_err(|err| {
                Failure::failed("engine_failed", format!("{}: {err}", engine.node.display()))
            })?;
        let to_helper = helper.stdin.take().expect("piped");
        let from_helper = helper.stdout.take().expect("piped");
        let mut browser = Camoufox {
            helper,
            to_helper,
            from_helper,
            buffer: Vec::new(),
            next_id: 0,
            browser: 0,
            current: String::new(),
            deadline: None,
            unanswered: false,
            seen: HashMap::new(),
            scratch: scratch.to_path_buf(),
            _lock: lock,
        };
        let mut env = json!({
            "PATH": PATH,
            "HOME": scratch,
            "TMPDIR": scratch.join("tmp"),
            "MOZ_CRASHREPORTER_DISABLE": "1",
        });
        if let Some(display) = display {
            env["DISPLAY"] = json!(display);
        }
        let launched = browser.request::<Value>(
            "launch",
            json!({
                "profile": profile.dir,
                "data": profile.data(),
                "downloads": scratch.join("downloads"),
                "executable": engine.executable,
                "headless": headless,
                "os": CAMOUFOX.os,
                "focus": CAMOUFOX.focus,
                "reader": READER,
                "env": env,
            }),
            STARTUP,
        );
        let found = launched.and_then(|_| {
            child_running(browser.helper.id(), &engine.executable)
                .ok_or_else(|| Failure::failed("engine_failed", "Camoufox did not start".into()))
        });
        match found {
            Ok(pid) => browser.browser = pid,
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
        params["session"] = json!(self.current);
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
            let active = answer["active"].as_str().unwrap_or_default();
            self.seen.insert(
                self.current.clone(),
                (answer["dialog"] == true, active.to_string()),
            );
            if let Some(error) = answer.get("error") {
                let code = error["code"].as_str().unwrap_or("engine_error");
                let message = error["message"].as_str().unwrap_or_default().to_string();
                let exit = match code {
                    "no_such_tab" | "last_tab" | "no_such_download" | "bad_request" => Exit::Usage,
                    _ => Exit::Failed,
                };
                return Err(Failure::new(exit, code, message));
            }
            return serde_json::from_value(answer["ok"].clone()).map_err(|err| {
                Failure::failed("engine_error", format!("the helper's {op} answer: {err}"))
            });
        }
    }

    fn read_line(&mut self, deadline: Instant, op: &str) -> Result<Value, Failure> {
        loop {
            if let Some(end) = self.buffer.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = self.buffer.drain(..=end).collect();
                return serde_json::from_slice(&line[..end]).map_err(|_| {
                    Failure::failed(
                        "engine_error",
                        "an unreadable answer from the helper".into(),
                    )
                });
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                self.unanswered = true;
                return Err(Failure::failed(
                    "timeout",
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

    /// Session `key`'s own folder in the runtime's scratch folder.
    fn session_folder(&self, key: &str) -> PathBuf {
        self.scratch.join("sessions").join(key)
    }

    fn frame_params(frame: &Frame) -> Value {
        json!({"frame": frame.id, "document": frame.document})
    }
}

impl Driver for Camoufox {
    fn limit(&mut self, deadline: Option<Instant>) {
        self.deadline = deadline;
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

    fn front(&mut self) -> Result<(), Failure> {
        self.ask::<Value>("front", json!({})).map(drop)
    }

    fn active_tab(&self) -> String {
        self.seen
            .get(&self.current)
            .map(|(_, active)| active.clone())
            .unwrap_or_default()
    }

    fn navigate(&mut self, url: &str) -> Result<(), Failure> {
        self.ask::<Value>("navigate", json!({"url": url})).map(drop)
    }

    fn back(&mut self) -> Result<bool, Failure> {
        self.ask("back", json!({}))
    }

    fn back_url(&mut self) -> Result<Option<String>, Failure> {
        self.ask("back_url", json!({}))
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
            return Err(Failure::usage(
                "bad_key",
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
        self.seen
            .get(&self.current)
            .is_some_and(|(dialog, _)| *dialog)
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
        self.session_folder(&self.current).join("uploads")
    }

    fn downloads(&mut self) -> Result<Vec<Download>, Failure> {
        self.ask("downloads", json!({}))
    }

    fn download_file(&mut self, id: &str) -> Result<(Download, PathBuf), Failure> {
        let found: Value = self.ask("download_file", json!({"download": id}))?;
        let download = serde_json::from_value(found["download"].clone())
            .map_err(|err| Failure::failed("engine_error", err.to_string()))?;
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
            .ok_or_else(|| Failure::failed("engine_error", "no screenshot".into()))?;
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
    fn close(&mut self) -> Value {
        let deadline = Instant::now() + CLOSING;
        let _ = self.request::<Value>("close", json!({}), CLOSING);
        let mut forced = false;
        while self.helper.try_wait().ok().flatten().is_none() {
            if Instant::now() >= deadline {
                forced = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
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
        let left = || groups.iter().any(|g| unsafe { libc::killpg(*g, 0) } == 0);
        while left() && Instant::now() < gone_by {
            std::thread::sleep(Duration::from_millis(50));
        }
        json!({"closed": !left(), "forced": forced})
    }
}

impl Runtime for Camoufox {
    fn begin(&mut self, key: &str) -> Result<(), Failure> {
        self.select(key);
        self.ask::<Value>("begin", json!({})).map(drop)
    }

    fn end(&mut self, key: &str) {
        self.select(key);
        let _ = self.ask::<Value>("end", json!({}));
        self.seen.remove(key);
        let _ = fs::remove_dir_all(self.session_folder(key));
    }

    fn select(&mut self, key: &str) {
        self.current = key.to_string();
    }

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
    Failure::new(Exit::Failed, "browser_gone", "the browser stopped".into())
}

fn io_failure(err: std::io::Error) -> Failure {
    Failure::failed("engine_failed", err.to_string())
}

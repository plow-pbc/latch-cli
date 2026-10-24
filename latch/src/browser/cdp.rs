//! The Chrome DevTools Protocol over `--remote-debugging-pipe`: Chrome
//! reads commands on its fd 3 and writes replies and events on its fd 4,
//! each message JSON ending in a NUL byte. The pipes are private to the
//! worker and Chrome; nothing listens on a port.

use std::collections::{HashSet, VecDeque};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::failure::{Failure, code};

/// The schemes whose loads the driver stops before they start: those that
/// read the host's own files or the browser's own pages, which no proxy
/// sees (the PDF viewer's extension pages are left alone). Requests of
/// other schemes are never paused.
pub const OFF_THE_WEB: &[&str] = &["file", "filesystem", "chrome", "devtools", "view-source"];

/// Whether `url` has a query parameter that holds one of `values` as a
/// fill takes a field to hold it (`keeps`). Line breaks count alike
/// however they are written: a form sends a field's as CRLF.
pub fn carries(url: &str, values: &[String]) -> bool {
    let lines = |text: &str| text.replace("\r\n", "\n").replace('\r', "\n");
    url::Url::parse(url).is_ok_and(|url| {
        url.query_pairs().any(|(_, value)| {
            values
                .iter()
                .any(|guarded| keeps(&lines(&value), &lines(guarded)))
        })
    })
}

/// The characters a field may add between a value's own as it formats it,
/// the same in `reader.js` and `helper.mjs`.
const SEPARATORS: &[char] = &[' ', '\u{a0}', '-', '/', '.', '(', ')', '+'];

/// Whether `held` holds `wanted`: its characters in order, with only
/// [`SEPARATORS`] between them (`reader.js`'s `keeps`).
fn keeps(held: &str, wanted: &str) -> bool {
    if held == wanted {
        return true;
    }
    if wanted.is_empty() {
        return false;
    }
    let mut want = wanted.chars().peekable();
    for ch in held.chars() {
        if want.peek() == Some(&ch) {
            want.next();
        } else if !SEPARATORS.contains(&ch) {
            return false;
        }
    }
    want.peek().is_none()
}

pub struct Cdp {
    to_chrome: File,
    from_chrome: File,
    buffer: Vec<u8>,
    next_id: u64,
    /// Events read while waiting for a reply, oldest first: those latch
    /// acts on (`queue`).
    events: VecDeque<Value>,
    /// Page targets, whose main frame's loading is kept.
    pages: HashSet<String>,
    /// Frames removed from their page since the events were last taken.
    removed: HashSet<String>,
    /// A reply did not come in time since this was last asked.
    unanswered: bool,
    /// Filled vault values no GET request may carry as a query parameter
    /// (`Driver::guard`), and whether a request carried one since this was
    /// last asked.
    pub guards: Vec<String>,
    pub stopped: bool,
}

/// The two pipes for one Chrome: the ends Chrome gets (to become its fd 3
/// and fd 4) and the ends the driver keeps. Every end is close-on-exec, so
/// no other program the worker starts inherits one.
pub struct Pipes {
    pub chrome_reads: OwnedFd,
    pub chrome_writes: OwnedFd,
    pub cdp: Cdp,
}

pub fn pipes() -> io::Result<Pipes> {
    let (chrome_reads, to_chrome) = pipe()?;
    let (from_chrome, chrome_writes) = pipe()?;
    Ok(Pipes {
        chrome_reads,
        chrome_writes,
        cdp: Cdp {
            to_chrome: File::from(to_chrome),
            from_chrome: File::from(from_chrome),
            buffer: Vec::new(),
            next_id: 0,
            events: VecDeque::new(),
            pages: HashSet::new(),
            removed: HashSet::new(),
            unanswered: false,
            guards: Vec::new(),
            stopped: false,
        },
    })
}

/// A pipe as (read end, write end), both close-on-exec.
fn pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0; 2];
    // SAFETY: `pipe` writes two descriptors into the array we own.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: both descriptors were just created and are owned by nobody else.
    let ends = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    for fd in [&ends.0, &ends.1] {
        // SAFETY: fcntl on a descriptor we own.
        if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(ends)
}

impl Cdp {
    /// Sends one command and waits for its reply until `deadline`.
    /// `session` addresses a page attached with `flatten`.
    pub fn call(
        &mut self,
        method: &str,
        params: Value,
        session: Option<&str>,
        deadline: Instant,
    ) -> Result<Value, Failure> {
        let id = self.send(method, params, session)?;
        self.reply_or(id, method, deadline, |_| false)
            .map(|reply| reply.expect("no stop event"))
    }

    /// Sends one command without waiting; its reply comes through `reply_or`.
    pub fn send(
        &mut self,
        method: &str,
        params: Value,
        session: Option<&str>,
    ) -> Result<u64, Failure> {
        self.next_id += 1;
        let id = self.next_id;
        let mut message = json!({"id": id, "method": method, "params": params});
        if let Some(session) = session {
            message["sessionId"] = json!(session);
        }
        let mut bytes = serde_json::to_vec(&message).expect("JSON");
        bytes.push(0);
        self.to_chrome.write_all(&bytes).map_err(|_| gone())?;
        Ok(id)
    }

    /// Waits for the reply to `id`, or for an event `stop` accepts, which
    /// stays queued: `Ok(None)`. Input that opens a dialog is only answered
    /// once the dialog is closed, so its caller stops at the dialog instead.
    /// A reply that comes after its caller stopped waiting is dropped.
    pub fn reply_or(
        &mut self,
        id: u64,
        method: &str,
        deadline: Instant,
        stop: impl Fn(&Value) -> bool,
    ) -> Result<Option<Value>, Failure> {
        if self.events.iter().any(&stop) {
            return Ok(None);
        }
        loop {
            let message = self.read(deadline, method).inspect_err(|failure| {
                self.unanswered |= failure.code == "timeout";
            })?;
            if message["id"] == id {
                if let Some(error) = message.get("error") {
                    return Err(Failure::new(
                        code::engine_error,
                        format!(
                            "{method}: {}",
                            error["message"].as_str().unwrap_or("failed")
                        ),
                    ));
                }
                return Ok(Some(message["result"].clone()));
            }
            if message.get("method").is_some() {
                let stopped = stop(&message);
                self.queue(message);
                if stopped {
                    return Ok(None);
                }
            }
        }
    }

    /// Reads whatever arrives within `wait` into the event queue.
    pub fn pump(&mut self, wait: Duration) -> Result<(), Failure> {
        let deadline = Instant::now() + wait;
        // What the browser sent already is taken in, with no time to wait too.
        if readable(&self.from_chrome, Duration::ZERO)? {
            self.take_chunk()?;
        }
        loop {
            match self.read(deadline, "events") {
                Ok(message) => {
                    if message.get("method").is_some() {
                        self.queue(message);
                    }
                }
                Err(failure) if failure.code == "timeout" => return Ok(()),
                Err(failure) => return Err(failure),
            }
        }
    }

    /// Whether a reply did not come in time since this was last asked.
    pub fn take_unanswered(&mut self) -> bool {
        std::mem::take(&mut self.unanswered)
    }

    /// The events read so far, oldest first, leaving none queued.
    pub fn take_events(&mut self) -> Vec<Value> {
        self.removed.clear();
        self.events.drain(..).collect()
    }

    /// Whether `frame` was removed from its page since the events were
    /// last taken: nothing sent to it would be answered.
    pub fn removed(&self, frame: &str) -> bool {
        self.removed.contains(frame)
    }

    /// Queues `event` if latch acts on it: what opens and closes tabs,
    /// frames in other processes, dialogs, downloads, and a tab's own
    /// loading. A page whose frames come and go sends thousands of other
    /// events a minute, which are dropped; a frame that attached and
    /// detached again before anyone looked is dropped too. Loading events
    /// are read for whether one came, so each is queued once per tab
    /// session and frame; a download keeps only its latest progress.
    fn queue(&mut self, event: Value) {
        let params = &event["params"];
        let text = |key: &str| params[key].as_str().unwrap_or_default().to_string();
        let keep = match event["method"].as_str().unwrap_or_default() {
            "Target.targetCreated" => {
                let page = params["targetInfo"]["type"] == "page";
                if page {
                    let target = params["targetInfo"]["targetId"]
                        .as_str()
                        .unwrap_or_default();
                    self.pages.insert(target.to_string());
                }
                page
            }
            "Target.targetDestroyed" => self.pages.remove(&text("targetId")),
            "Target.attachedToTarget" => params["targetInfo"]["type"] == "iframe",
            "Target.detachedFromTarget" => {
                let session = text("sessionId");
                let attached = self.events.iter().position(|queued| {
                    queued["method"] == "Target.attachedToTarget"
                        && queued["params"]["sessionId"] == session.as_str()
                });
                match attached {
                    Some(at) => {
                        self.events.remove(at);
                        false
                    }
                    None => true,
                }
            }
            "Page.frameDetached" => {
                // A frame moving to another process is not removed.
                if params["reason"] != "swap" {
                    self.removed.insert(text("frameId"));
                }
                false
            }
            method @ ("Page.frameStartedLoading"
            | "Page.frameRequestedNavigation"
            | "Page.frameStoppedLoading"
            | "Page.loadEventFired") => {
                let tab = method == "Page.loadEventFired" || self.pages.contains(&text("frameId"));
                tab && !self.events.iter().any(|queued| {
                    queued["method"] == method
                        && queued["sessionId"] == event["sessionId"]
                        && queued["params"]["frameId"] == params["frameId"]
                })
            }
            "Browser.downloadProgress" => {
                let earlier = self.events.iter_mut().find(|queued| {
                    queued["method"] == "Browser.downloadProgress"
                        && queued["params"]["guid"] == params["guid"]
                });
                match earlier {
                    Some(earlier) => {
                        *earlier = event;
                        return;
                    }
                    None => true,
                }
            }
            // A load of a scheme off the web never goes on; while a guard
            // stands, every request is paused, and a GET carrying a guarded
            // value is stopped too.
            "Fetch.requestPaused" => {
                let request = &params["request"];
                let url = request["url"].as_str().unwrap_or_default();
                let web = url.starts_with("http://") || url.starts_with("https://");
                let carries = request["method"] == "GET" && carries(url, &self.guards);
                let session = event["sessionId"].as_str();
                if web && !carries {
                    let _ = self.send(
                        "Fetch.continueRequest",
                        json!({"requestId": params["requestId"]}),
                        session,
                    );
                } else {
                    self.stopped |= carries;
                    // An aborted navigation commits no error page, which
                    // would keep the address, value and all, as the tab's.
                    let reason = if carries {
                        "Aborted"
                    } else {
                        "BlockedByClient"
                    };
                    let fail = json!({"requestId": params["requestId"], "errorReason": reason});
                    let _ = self.send("Fetch.failRequest", fail, session);
                }
                false
            }
            // Only a page's own load, XHR or fetch the site refused is
            // kept, of all the responses its pages get.
            "Network.responseReceived" => {
                matches!(params["type"].as_str(), Some("Document" | "XHR" | "Fetch"))
                    && params["response"]["status"].as_u64().unwrap_or(0) >= 400
            }
            // An open dialog blocks its page until latch answers it, so
            // these come one at a time.
            "Page.javascriptDialogOpening"
            | "Page.javascriptDialogClosed"
            | "Browser.downloadWillBegin" => true,
            _ => false,
        };
        if keep {
            self.events.push_back(event);
        }
    }

    /// The next whole message, waiting no later than `deadline`.
    fn read(&mut self, deadline: Instant, waiting_for: &str) -> Result<Value, Failure> {
        loop {
            if let Some(end) = self.buffer.iter().position(|&b| b == 0) {
                let message: Vec<u8> = self.buffer.drain(..=end).collect();
                return serde_json::from_slice(&message[..end]).map_err(|_| {
                    Failure::new(code::engine_error, "unreadable CDP message".into())
                });
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() || !readable(&self.from_chrome, left)? {
                return Err(Failure::new(
                    code::timeout,
                    format!("the browser did not answer {waiting_for} in time"),
                ));
            }
            self.take_chunk()?;
        }
    }

    /// Reads what the pipe has, which must be readable.
    fn take_chunk(&mut self) -> Result<(), Failure> {
        let mut chunk = [0u8; 64 * 1024];
        let n = self.from_chrome.read(&mut chunk).map_err(|_| gone())?;
        if n == 0 {
            return Err(gone());
        }
        self.buffer.extend_from_slice(&chunk[..n]);
        Ok(())
    }
}

/// Whether `file` has bytes (or its end) to read within `wait`.
pub fn readable(file: &impl AsRawFd, wait: Duration) -> Result<bool, Failure> {
    let mut poll = libc::pollfd {
        fd: file.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let millis = wait.as_millis().min(i32::MAX as u128) as i32;
    loop {
        // SAFETY: one pollfd we own, for the duration of the call.
        let ready = unsafe { libc::poll(&mut poll, 1, millis.max(1)) };
        if ready >= 0 {
            return Ok(ready > 0);
        }
        if io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return Err(Failure::new(code::engine_error, "poll failed".into()));
        }
    }
}

fn gone() -> Failure {
    Failure::new(code::browser_gone, "the browser closed its pipe".into())
}

/// Standard base64, as CDP sends screenshots.
pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    fn value(c: u8) -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    }
    let bytes = text.trim_end_matches('=').as_bytes();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        let mut n = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            n |= value(c)? << (18 - 6 * i);
        }
        match chunk.len() {
            4 => out.extend_from_slice(&[(n >> 16) as u8, (n >> 8) as u8, n as u8]),
            3 => out.extend_from_slice(&[(n >> 16) as u8, (n >> 8) as u8]),
            2 => out.push((n >> 16) as u8),
            _ => return None,
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_guarded_value_is_found_only_as_a_whole_query_value() {
        let guard = vec!["pa ss&w%rd".to_string()];
        let found = |url: &str| carries(url, &guard);
        assert!(found("https://a.example/w?u=ada&p=pa+ss%26w%25rd"));
        assert!(found("https://a.example/w?p=pa%20ss%26w%25rd#top"));
        assert!(!found("https://a.example/w?p=pa+ss%26w%25rd2"));
        assert!(!found("https://a.example/pa ss&w%rd"));
        assert!(!carries("https://a.example/w?p=x", &[]));
        // A form sends a field's line breaks as CRLF.
        let lines = vec!["line one\nline two".to_string()];
        assert!(carries(
            "https://a.example/w?note=line+one%0D%0Aline+two",
            &lines
        ));
        // A code as its field formats it, but not another number, nor one
        // with a character no field adds (which a fill doesn't take as
        // held either); a secret of separators alone, formatted.
        let code = vec!["123456".to_string()];
        assert!(carries("https://a.example/w?otp=123+456", &code));
        assert!(carries("https://a.example/w?otp=123-456", &code));
        assert!(!carries("https://a.example/w?otp=123+457", &code));
        assert!(!carries("https://a.example/w?otp=123%EF%BB%BF456", &code));
        assert!(carries(
            "https://a.example/w?p=--+--",
            &["----".to_string()]
        ));
    }

    /// While a guard stands, Chrome pauses every request: a GET carrying a
    /// guarded secret is aborted and reported, the rest go on, and a scheme
    /// off the web is still refused.
    #[test]
    fn a_paused_request_is_checked_against_the_guard() {
        let Pipes {
            chrome_reads,
            chrome_writes,
            mut cdp,
        } = pipes().unwrap();
        cdp.guards = vec!["secret-a".into(), "secret-b".into()];
        let paused = |id: &str, method: &str, url: &str| {
            let event = json!({"method": "Fetch.requestPaused", "sessionId": "s",
                               "params": {"requestId": id, "request": {"method": method, "url": url}}});
            format!("{event}\0")
        };
        let mut chrome_out = File::from(chrome_writes);
        for (id, method, url) in [
            ("r1", "GET", "https://shop.example/login?pw=secret-a"),
            ("r2", "GET", "https://shop.example/logo.png?v=2"),
            ("r3", "POST", "https://shop.example/login?pw=secret-b"),
            ("r4", "GET", "file:///etc/hosts"),
        ] {
            chrome_out
                .write_all(paused(id, method, url).as_bytes())
                .unwrap();
        }
        cdp.pump(Duration::from_millis(200)).unwrap();
        let mut sent = vec![0u8; 4096];
        let n = File::from(chrome_reads).read(&mut sent).unwrap();
        let sent: Vec<Value> = sent[..n]
            .split(|&b| b == 0)
            .filter(|m| !m.is_empty())
            .map(|m| serde_json::from_slice(m).unwrap())
            .collect();
        let answer = |id: &str| {
            let m = sent
                .iter()
                .find(|m| m["params"]["requestId"] == id)
                .unwrap();
            (
                m["method"].as_str().unwrap().to_string(),
                m["params"]["errorReason"].clone(),
            )
        };
        assert_eq!(answer("r1"), ("Fetch.failRequest".into(), json!("Aborted")));
        assert_eq!(answer("r2"), ("Fetch.continueRequest".into(), Value::Null));
        assert_eq!(answer("r3"), ("Fetch.continueRequest".into(), Value::Null));
        assert_eq!(
            answer("r4"),
            ("Fetch.failRequest".into(), json!("BlockedByClient"))
        );
        assert!(cdp.stopped);
    }

    #[test]
    fn base64_decodes_every_padding() {
        assert_eq!(base64_decode("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(base64_decode("aGk=").unwrap(), b"hi");
        assert_eq!(base64_decode("YWJj").unwrap(), b"abc");
        assert_eq!(base64_decode("").unwrap(), b"");
        assert_eq!(base64_decode("+/+/").unwrap(), [0xfb, 0xff, 0xbf]);
        assert!(base64_decode("a").is_none());
        assert!(base64_decode("a$==").is_none());
    }

    #[test]
    fn replies_and_events_are_split_on_nul_and_matched_by_id() {
        let Pipes {
            chrome_reads,
            chrome_writes,
            mut cdp,
        } = pipes().unwrap();
        let mut chrome_out = File::from(chrome_writes);
        chrome_out
            .write_all(b"{\"method\":\"Page.loadEventFired\",\"sessionId\":\"s\"}\0{\"id\":1,\"result\":{\"ok\":true}}\0")
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let result = cdp
            .call("Page.navigate", json!({}), Some("s"), deadline)
            .unwrap();
        assert_eq!(result, json!({"ok": true}));
        let mut sent = vec![0u8; 256];
        let mut chrome_in = File::from(chrome_reads);
        let n = chrome_in.read(&mut sent).unwrap();
        assert!(sent[..n].ends_with(b"\0"));
        let events = cdp.take_events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["method"], "Page.loadEventFired");
        // Nothing more within the wait is not an error, and queues nothing.
        cdp.pump(Duration::from_millis(50)).unwrap();
        assert!(cdp.take_events().is_empty());
        // A reply that does not come in time is.
        let id = cdp.send("Page.reload", json!({}), Some("s")).unwrap();
        let soon = Instant::now() + Duration::from_millis(50);
        let late = cdp.reply_or(id, "Page.reload", soon, |_| false);
        assert_eq!(late.err().unwrap().code, "timeout");
        // An event the caller stops at ends the wait, and stays queued.
        chrome_out
            .write_all(b"{\"method\":\"Page.javascriptDialogOpening\",\"sessionId\":\"s\"}\0")
            .unwrap();
        let dialog = |e: &Value| e["method"] == "Page.javascriptDialogOpening";
        let stopped = cdp.reply_or(id, "Page.reload", deadline, dialog).unwrap();
        assert!(stopped.is_none());
        assert_eq!(cdp.take_events().len(), 1);
    }

    #[test]
    fn a_page_churning_frames_queues_only_what_latch_acts_on() {
        let Pipes { mut cdp, .. } = pipes().unwrap();
        let event = |method: &str, params: Value| json!({"method": method, "params": params});
        cdp.queue(event(
            "Target.targetCreated",
            json!({"targetInfo": {"type": "page", "targetId": "TAB"}}),
        ));
        // Ten thousand frames, each loading, attached in another process
        // and gone again before latch looked.
        for n in 0..10_000 {
            let frame = format!("F{n}");
            let session = format!("S{n}");
            for method in [
                "Page.frameAttached",
                "Page.frameStartedLoading",
                "Page.frameNavigated",
                "Page.frameStoppedLoading",
            ] {
                cdp.queue(event(method, json!({"frameId": frame})));
            }
            cdp.queue(event(
                "Target.attachedToTarget",
                json!({"sessionId": session, "targetInfo": {"type": "iframe", "targetId": frame}}),
            ));
            cdp.queue(event(
                "Target.detachedFromTarget",
                json!({"sessionId": session}),
            ));
            cdp.queue(event(
                "Page.frameDetached",
                json!({"frameId": frame, "reason": "remove"}),
            ));
        }
        // One frame still there, and the tab's own load.
        cdp.queue(event(
            "Target.attachedToTarget",
            json!({"sessionId": "S", "targetInfo": {"type": "iframe", "targetId": "F"}}),
        ));
        cdp.queue(event("Page.frameStoppedLoading", json!({"frameId": "TAB"})));
        cdp.queue(event(
            "Page.frameDetached",
            json!({"frameId": "MOVED", "reason": "swap"}),
        ));
        assert!(cdp.removed("F9999"));
        assert!(!cdp.removed("MOVED"));
        let methods: Vec<Value> = cdp
            .take_events()
            .iter()
            .map(|e| e["method"].clone())
            .collect();
        assert_eq!(
            methods,
            [
                "Target.targetCreated",
                "Target.attachedToTarget",
                "Page.frameStoppedLoading"
            ]
        );
        assert!(!cdp.removed("F9999"));
    }

    #[test]
    fn repeated_loads_and_download_progress_queue_once_each() {
        let Pipes { mut cdp, .. } = pipes().unwrap();
        let event = |method: &str, session: &str, params: Value| json!({"method": method, "sessionId": session, "params": params});
        cdp.queue(event(
            "Target.targetCreated",
            "",
            json!({"targetInfo": {"type": "page", "targetId": "TAB"}}),
        ));
        cdp.queue(event(
            "Browser.downloadWillBegin",
            "",
            json!({"guid": "G", "frameId": "TAB"}),
        ));
        // A page reloading itself and a download reporting progress, ten
        // thousand times each.
        for n in 0..10_000 {
            for method in [
                "Page.frameStartedLoading",
                "Page.frameRequestedNavigation",
                "Page.frameStoppedLoading",
            ] {
                cdp.queue(event(method, "S", json!({"frameId": "TAB"})));
            }
            cdp.queue(event("Page.loadEventFired", "S", json!({})));
            cdp.queue(event(
                "Browser.downloadProgress",
                "",
                json!({"guid": "G", "receivedBytes": n, "state": "inProgress"}),
            ));
        }
        cdp.queue(event(
            "Browser.downloadProgress",
            "",
            json!({"guid": "G", "receivedBytes": 10_000, "state": "completed"}),
        ));
        let events = cdp.take_events();
        let methods: Vec<&str> = events
            .iter()
            .map(|e| e["method"].as_str().unwrap())
            .collect();
        assert_eq!(
            methods,
            [
                "Target.targetCreated",
                "Browser.downloadWillBegin",
                "Page.frameStartedLoading",
                "Page.frameRequestedNavigation",
                "Page.frameStoppedLoading",
                "Page.loadEventFired",
                "Browser.downloadProgress",
            ]
        );
        // The download's latest progress is the one kept.
        assert_eq!(events[6]["params"]["state"], "completed");
    }
}

//! The Chrome DevTools Protocol over `--remote-debugging-pipe`: Chrome
//! reads commands on its fd 3 and writes replies and events on its fd 4,
//! each message JSON ending in a NUL byte. The pipes are private to the
//! worker and Chrome; nothing listens on a port.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::failure::Failure;

pub struct Cdp {
    to_chrome: File,
    from_chrome: File,
    buffer: Vec<u8>,
    next_id: u64,
    /// Events read while waiting for a reply, oldest first.
    events: VecDeque<Value>,
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
            let message = self.read(deadline, method)?;
            if message["id"] == id {
                if let Some(error) = message.get("error") {
                    return Err(Failure::failed(
                        "engine_error",
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
                self.events.push_back(message);
                if stopped {
                    return Ok(None);
                }
            }
        }
    }

    /// Reads whatever arrives within `wait` into the event queue.
    pub fn pump(&mut self, wait: Duration) -> Result<(), Failure> {
        let deadline = Instant::now() + wait;
        loop {
            match self.read(deadline, "events") {
                Ok(message) => {
                    if message.get("method").is_some() {
                        self.events.push_back(message);
                    }
                }
                Err(failure) if failure.code == "timeout" => return Ok(()),
                Err(failure) => return Err(failure),
            }
        }
    }

    /// The events read so far, oldest first, leaving none queued.
    pub fn take_events(&mut self) -> Vec<Value> {
        self.events.drain(..).collect()
    }

    /// The next whole message, waiting no later than `deadline`.
    fn read(&mut self, deadline: Instant, waiting_for: &str) -> Result<Value, Failure> {
        loop {
            if let Some(end) = self.buffer.iter().position(|&b| b == 0) {
                let message: Vec<u8> = self.buffer.drain(..=end).collect();
                return serde_json::from_slice(&message[..end])
                    .map_err(|_| Failure::failed("engine_error", "unreadable CDP message".into()));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() || !readable(&self.from_chrome, left)? {
                return Err(Failure::failed(
                    "timeout",
                    format!("the browser did not answer {waiting_for} in time"),
                ));
            }
            let mut chunk = [0u8; 64 * 1024];
            let n = self.from_chrome.read(&mut chunk).map_err(|_| gone())?;
            if n == 0 {
                return Err(gone());
            }
            self.buffer.extend_from_slice(&chunk[..n]);
        }
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
            return Err(Failure::failed("engine_error", "poll failed".into()));
        }
    }
}

fn gone() -> Failure {
    Failure::failed("browser_gone", "the browser closed its pipe".into())
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
}

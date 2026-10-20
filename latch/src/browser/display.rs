//! A screenless Linux host's private display: one Xvfb server per profile
//! runtime, started by its worker before the engine and stopped with it.
//! The runtime's sessions are windows on it; closing one session leaves it
//! up while another still uses the runtime, and each runtime has its own.
//!
//! Xvfb picks a free display number itself (`-displayfd`) and listens on no
//! TCP port. A Linux host with a screen passes its own `DISPLAY` instead;
//! a Mac needs neither.

use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::failure::Failure;

/// What the host asks the worker for: `private` for a display of its own.
pub const PRIVATE: &str = "private";

/// The display a headed runtime's engine uses: none on a Mac; on Linux,
/// the host's own (`DISPLAY`), or a private one when it has none.
pub fn for_runtime(headless: bool) -> Option<String> {
    if headless || cfg!(target_os = "macos") {
        return None;
    }
    Some(
        std::env::var("DISPLAY")
            .ok()
            .filter(|display| !display.is_empty())
            .unwrap_or_else(|| PRIVATE.to_string()),
    )
}

/// How long Xvfb may take to say which display it took.
const READY: Duration = Duration::from_secs(10);

pub struct Display {
    child: Child,
    /// `:N`, for the engine's `DISPLAY`.
    pub name: String,
}

impl Display {
    /// Starts Xvfb on a free display, and waits until it is ready.
    pub fn start() -> Result<Display, Failure> {
        let failed = |why: String| Failure::failed("display_failed", why);
        let mut fds = [0; 2];
        // SAFETY: `pipe` fills both fds, which are owned from here on.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(failed(std::io::Error::last_os_error().to_string()));
        }
        let (reads, writes) =
            unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        let write_fd = writes.as_raw_fd();
        let mut command = Command::new("Xvfb");
        command
            .args(arguments(write_fd))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        // SAFETY: only async-signal-safe calls: the read end is closed in
        // the child, so the parent sees the end of the pipe if Xvfb dies.
        let read_fd = reads.as_raw_fd();
        unsafe {
            command.pre_exec(move || {
                libc::close(read_fd);
                Ok(())
            });
        }
        let child = command
            .spawn()
            .map_err(|err| failed(format!("Xvfb could not start: {err}")))?;
        drop(writes);
        let mut display = Display {
            child,
            name: String::new(),
        };
        let mut reads = std::fs::File::from(reads);
        let deadline = Instant::now() + READY;
        let mut said = Vec::new();
        let mut byte = [0u8; 1];
        while Instant::now() < deadline {
            match reads.read(&mut byte) {
                Ok(1) if byte[0] == b'\n' => break,
                Ok(1) => said.push(byte[0]),
                _ => return Err(failed("Xvfb stopped before it was ready".into())),
            }
        }
        display.name = display_name(&said)
            .ok_or_else(|| failed("Xvfb did not say which display it took".into()))?;
        Ok(display)
    }
}

impl Drop for Display {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Xvfb's command line: one screen, no TCP, and the display number
/// written to `fd` once it is ready.
fn arguments(fd: i32) -> Vec<String> {
    [
        "-displayfd",
        &fd.to_string(),
        "-screen",
        "0",
        "1280x800x24",
        "-nolisten",
        "tcp",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// `:N` from what Xvfb wrote to its display fd.
fn display_name(said: &[u8]) -> Option<String> {
    let number = std::str::from_utf8(said).ok()?.trim();
    number.parse::<u32>().ok().map(|n| format!(":{n}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xvfb_takes_a_free_display_and_no_tcp_port() {
        let args = arguments(7);
        assert_eq!(args[..2], ["-displayfd", "7"]);
        assert!(args.windows(2).any(|w| w == ["-nolisten", "tcp"]));
        assert_eq!(display_name(b"12"), Some(":12".into()));
        assert_eq!(display_name(b"x"), None);
    }
}

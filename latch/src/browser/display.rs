//! A Linux host's private display: one Xvfb server per headed session
//! runtime, started by its worker before the engine and stopped with it.
//! The runtime's sessions are windows on it; closing one session leaves it
//! up while another still uses the runtime, and each runtime has its own.
//!
//! Xvfb picks a free display number itself (`-displayfd`) and listens only
//! on its socket file in the worker's private `/tmp`: no TCP port, and no
//! abstract socket, which anything sharing the host's network could reach.
//! The engine is given that file's path, never `:N`, which X clients look
//! up as an abstract socket first and would find another server's there.
//! A Linux host never gives the engine its own `DISPLAY`, screen or not; a
//! Mac needs no display.

use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::failure::{Failure, code};

/// Whether a runtime's worker starts a private display: on Linux, for
/// every headed runtime.
pub fn for_runtime(headless: bool) -> bool {
    !headless && cfg!(target_os = "linux")
}

/// How long Xvfb may take to say which display it took.
const READY: Duration = Duration::from_secs(10);

pub struct Display {
    child: Child,
    /// The display's socket file, for the engine's `DISPLAY`.
    pub name: String,
}

impl Display {
    /// Starts Xvfb on a free display, and waits until it is ready.
    pub fn start() -> Result<Display, Failure> {
        let failed = |why: String| Failure::new(code::display_failed, why);
        let mut fds = [0; 2];
        // SAFETY: `pipe` fills both fds, which are owned from here on.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(failed(std::io::Error::last_os_error().to_string()));
        }
        // SAFETY: both fds were just created by `pipe` and nothing else owns them.
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
        // Gone with the worker, however it ends.
        bound(&mut command);
        let read_fd = reads.as_raw_fd();
        // SAFETY: only async-signal-safe calls: the read end is closed in
        // the child, so the parent sees the end of the pipe if Xvfb dies.
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

/// Ties `command`'s process to this one: it is killed when this process
/// ends, even by SIGKILL, when no `Drop` runs.
fn bound(command: &mut Command) {
    #[cfg(target_os = "linux")]
    {
        let parent = std::process::id() as libc::pid_t;
        // SAFETY: only async-signal-safe calls between fork and exec.
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                // The parent may have ended before the signal was set.
                if libc::getppid() != parent {
                    libc::_exit(1);
                }
                Ok(())
            });
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = command;
}

/// Xvfb's command line: one screen, no TCP or abstract socket, and the
/// display number written to `fd` once it is ready.
fn arguments(fd: i32) -> Vec<String> {
    [
        "-displayfd",
        &fd.to_string(),
        "-screen",
        "0",
        "1280x800x24",
        "-nolisten",
        "tcp",
        "-nolisten",
        "local",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Display N's socket file, from what Xvfb wrote to its display fd.
fn display_name(said: &[u8]) -> Option<String> {
    let number = std::str::from_utf8(said).ok()?.trim();
    number
        .parse::<u32>()
        .ok()
        .map(|n| format!("/tmp/.X11-unix/X{n}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in for a worker: started by the test below with
    /// `LATCH_TEST_BOUND_HELPER` naming a file, it starts a bound `sleep`,
    /// writes its pid there, and waits to be killed. Run on its own, it
    /// does nothing.
    #[cfg(target_os = "linux")]
    #[test]
    fn bound_helper() {
        let Some(file) = std::env::var_os("LATCH_TEST_BOUND_HELPER") else {
            return;
        };
        let mut sleep = Command::new("sleep");
        sleep.arg("60").stdout(Stdio::null());
        bound(&mut sleep);
        let mut child = sleep.spawn().unwrap();
        std::fs::write(file, child.id().to_string()).unwrap();
        // Killed long before this: the test is about what happens then.
        let _ = child.wait();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_bound_process_ends_with_its_parent_even_on_sigkill() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("pid");
        let mut helper = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "browser::display::tests::bound_helper"])
            .env("LATCH_TEST_BOUND_HELPER", &file)
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        let pid: u32 = loop {
            if let Some(pid) = std::fs::read_to_string(&file)
                .ok()
                .and_then(|p| p.parse().ok())
            {
                break pid;
            }
            assert!(
                Instant::now() < deadline,
                "the helper never started its child"
            );
            std::thread::sleep(Duration::from_millis(50));
        };
        let alive = || {
            std::path::Path::new(&format!("/proc/{pid}")).exists()
                && !std::fs::read_to_string(format!("/proc/{pid}/stat"))
                    .unwrap_or_default()
                    .contains(") Z ")
        };
        assert!(alive());
        helper.kill().unwrap();
        helper.wait().unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while alive() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        let gone = !alive();
        if !gone {
            // SAFETY: a signal only, to the test's own leftover process.
            unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        }
        assert!(gone, "the bound process outlived its killed parent");
    }

    #[test]
    fn xvfb_takes_a_free_display_and_no_tcp_or_abstract_socket() {
        let args = arguments(7);
        assert_eq!(args[..2], ["-displayfd", "7"]);
        assert!(args.windows(2).any(|w| w == ["-nolisten", "tcp"]));
        assert!(args.windows(2).any(|w| w == ["-nolisten", "local"]));
        assert_eq!(display_name(b"12"), Some("/tmp/.X11-unix/X12".into()));
        assert_eq!(display_name(b"x"), None);
    }
}

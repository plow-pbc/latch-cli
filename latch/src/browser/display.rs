//! A Linux host's private display: one Xvfb server per headed session
//! runtime, started by its worker before the engine and stopped with it.
//! The runtime's sessions are windows on it; closing one session leaves it
//! up while another still uses the runtime, and each runtime has its own.
//!
//! Xvfb picks a free display number itself (`-displayfd`) and listens on
//! its socket file in the worker's private `/tmp` and on the abstract
//! socket of the same name, never on TCP. The engine is given `:N`, which
//! X clients look up as that abstract socket first: Xvfb holding it means
//! no other server's can answer, since `-displayfd` skips a number another
//! server holds. A socket file's path as `DISPLAY` would avoid the abstract
//! socket, but Ubuntu 24.04's libxcb (1.15) can't parse one. Anything
//! sharing the host's network can reach the abstract socket, so Xvfb lets
//! in only a client with the runtime's cookie (`XAUTHORITY`), which only the
//! worker and its engine see. A Linux host never gives the engine its own
//! `DISPLAY`, screen or not; a Mac needs no display.

use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::NamedTempFile;

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
    /// The display, `:N`.
    name: String,
    /// The Xauthority file with the cookie Xvfb asks for.
    cookie: NamedTempFile,
}

impl Display {
    /// Starts Xvfb on a free display, and waits until it is ready.
    pub fn start() -> Result<Display, Failure> {
        let failed = |why: String| Failure::new(code::display_failed, why);
        let cookie = cookie_file()
            .map_err(|err| failed(format!("the display's cookie could not be written: {err}")))?;
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
            .args(arguments(write_fd, cookie.path()))
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
            cookie,
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

    /// What the engine's environment needs to reach the display: `DISPLAY`
    /// and `XAUTHORITY`.
    pub fn env(&self) -> [(&'static str, String); 2] {
        [
            ("DISPLAY", self.name.clone()),
            ("XAUTHORITY", self.cookie.path().display().to_string()),
        ]
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

/// Xvfb's command line: one screen, no TCP, only clients with the cookie in
/// `cookie`, and the display number written to `fd` once it is ready. No
/// reset when its last client leaves: a server resetting drops a client
/// that connects meanwhile, such as an engine's next connection.
fn arguments(fd: i32, cookie: &Path) -> Vec<String> {
    [
        "-displayfd",
        &fd.to_string(),
        "-auth",
        &cookie.display().to_string(),
        "-screen",
        "0",
        "1280x800x24",
        "-nolisten",
        "tcp",
        "-noreset",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// An Xauthority file of one entry: a random MIT cookie for any address and
/// any display number, so it can be written before Xvfb picks one. Xvfb
/// reads its cookie from it, and the engine's X library what to show.
fn cookie_file() -> std::io::Result<NamedTempFile> {
    fn field(bytes: &[u8]) -> Vec<u8> {
        let length = u16::try_from(bytes.len()).expect("a short field");
        [&length.to_be_bytes(), bytes].concat()
    }
    const FAMILY_WILD: u16 = 0xffff;
    let entry = [
        FAMILY_WILD.to_be_bytes().to_vec(),
        field(b""),
        field(b""),
        field(b"MIT-MAGIC-COOKIE-1"),
        field(&rand::random::<[u8; 16]>()),
    ]
    .concat();
    let mut file = NamedTempFile::new()?;
    file.write_all(&entry)?;
    Ok(file)
}

/// Display N, `:N`, from what Xvfb wrote to its display fd.
fn display_name(said: &[u8]) -> Option<String> {
    let number = std::str::from_utf8(said).ok()?.trim();
    number.parse::<u32>().ok().map(|n| format!(":{n}"))
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
        // One read of its state, after its name: a process that is gone, a
        // zombie (Z), or one being reaped (X, seen just after Z) has ended.
        let alive = || {
            std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
                stat.rsplit_once(") ")
                    .is_some_and(|(_, state)| !state.starts_with(['Z', 'X']))
            })
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
    fn xvfb_takes_a_free_display_with_a_cookie_and_no_tcp() {
        let args = arguments(7, Path::new("/tmp/cookie"));
        assert_eq!(args[..4], ["-displayfd", "7", "-auth", "/tmp/cookie"]);
        assert!(args.windows(2).any(|w| w == ["-nolisten", "tcp"]));
        assert!(args.iter().any(|arg| arg == "-noreset"));
        assert_eq!(display_name(b"12"), Some(":12".into()));
        assert_eq!(display_name(b"x"), None);
    }
}

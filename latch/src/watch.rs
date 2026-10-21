//! Waiting, without polling, until something in a state folder may have
//! changed: a file in it was written, created, renamed or removed, a
//! process (the host) exited, or our stdin closed. The kernel wakes us
//! (kqueue on macOS, inotify and a pidfd on Linux); with nothing happening
//! the process sleeps. A wake says only "look again": callers re-read
//! what they follow and compare. So nothing slips between a look and the
//! wait, a caller arms the watch, then looks, then waits.

use std::io;
use std::path::{Path, PathBuf};

pub enum Woke {
    Changed,
    /// Our stdin was a pipe and its writer closed it: whoever started us is gone.
    StdinClosed,
}

/// Whether stdin is a pipe or socket, so its closing means the parent went
/// away. A terminal or `/dev/null` is not watched.
pub fn stdin_is_a_pipe() -> bool {
    // SAFETY: fstat only writes into `stat`.
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(0, &mut stat) } != 0 {
        return false;
    }
    let kind = stat.st_mode & libc::S_IFMT;
    kind == libc::S_IFIFO || kind == libc::S_IFSOCK
}

fn check(result: libc::c_int) -> io::Result<libc::c_int> {
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(result)
    }
}

/// Reads and drops what stdin has; true once it is closed.
fn stdin_closed() -> bool {
    let mut buffer = [0u8; 512];
    // SAFETY: reads into `buffer`, at most its length.
    let read = unsafe { libc::read(0, buffer.as_mut_ptr().cast(), buffer.len()) };
    read == 0 || (read < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted)
}

#[cfg(target_os = "macos")]
pub use macos::Watch;

#[cfg(target_os = "linux")]
pub use linux::Watch;

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    /// A kqueue: the folder itself (an entry added, renamed or removed),
    /// each of `files` (written to), the host process, and stdin.
    pub struct Watch {
        queue: libc::c_int,
        dir: PathBuf,
        files: Vec<PathBuf>,
        /// Open descriptors for the folder and the files; closing one drops
        /// its events, so each wait opens them afresh.
        open: Vec<libc::c_int>,
        process: Option<u32>,
        /// A change seen while arming (the process was already gone).
        pending: bool,
    }

    impl Watch {
        pub fn new(dir: &Path, files: &[PathBuf], stdin: bool) -> io::Result<Watch> {
            // SAFETY: no arguments; the result is checked.
            let queue = check(unsafe { libc::kqueue() })?;
            let watch = Watch {
                queue,
                dir: dir.to_path_buf(),
                files: files.to_vec(),
                open: Vec::new(),
                process: None,
                pending: false,
            };
            if stdin {
                watch.add(0, libc::EVFILT_READ, 0, libc::EV_ADD)?;
            }
            Ok(watch)
        }

        /// Also wakes when process `pid` exits; `None` stops watching one.
        pub fn process(&mut self, pid: Option<u32>) {
            if pid == self.process {
                return;
            }
            self.process = pid;
            if let Some(pid) = pid {
                let flags = libc::EV_ADD | libc::EV_ONESHOT;
                // A process that is already gone: look again at once.
                if self
                    .add(pid as usize, libc::EVFILT_PROC, libc::NOTE_EXIT, flags)
                    .is_err()
                {
                    self.pending = true;
                }
            }
        }

        /// Blocks until something changed since `arm`.
        pub fn wait(&mut self) -> io::Result<Woke> {
            if std::mem::take(&mut self.pending) {
                return Ok(Woke::Changed);
            }
            loop {
                // SAFETY: an all-zero kevent is a valid value to overwrite.
                let mut event: libc::kevent = unsafe { std::mem::zeroed() };
                // SAFETY: room for one event; no timeout, so it blocks.
                let got = unsafe {
                    libc::kevent(
                        self.queue,
                        std::ptr::null(),
                        0,
                        &mut event,
                        1,
                        std::ptr::null(),
                    )
                };
                if got < 0 {
                    let err = io::Error::last_os_error();
                    if err.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(err);
                }
                if event.filter == libc::EVFILT_READ && event.ident == 0 {
                    if event.flags & libc::EV_EOF != 0 || stdin_closed() {
                        return Ok(Woke::StdinClosed);
                    }
                    continue;
                }
                if event.filter == libc::EVFILT_PROC {
                    self.process = None;
                }
                return Ok(Woke::Changed);
            }
        }

        /// Watches the folder and the files as they are now, so a file
        /// created or replaced since the last wait is watched too.
        pub fn arm(&mut self) -> io::Result<()> {
            for fd in self.open.drain(..) {
                // SAFETY: a descriptor this watch opened.
                unsafe { libc::close(fd) };
            }
            let vnode = libc::NOTE_WRITE
                | libc::NOTE_EXTEND
                | libc::NOTE_DELETE
                | libc::NOTE_RENAME
                | libc::NOTE_ATTRIB;
            let paths: Vec<PathBuf> = std::iter::once(self.dir.clone())
                .chain(self.files.iter().cloned())
                .collect();
            for (index, path) in paths.iter().enumerate() {
                let name = CString::new(path.as_os_str().as_bytes())?;
                // SAFETY: a NUL-terminated path; the result is checked.
                let fd = unsafe { libc::open(name.as_ptr(), libc::O_EVTONLY | libc::O_CLOEXEC) };
                if fd < 0 {
                    // A file that does not exist yet: the folder's event says when it does.
                    if index == 0 {
                        return Err(io::Error::last_os_error());
                    }
                    continue;
                }
                self.open.push(fd);
                self.add(
                    fd as usize,
                    libc::EVFILT_VNODE,
                    vnode,
                    libc::EV_ADD | libc::EV_CLEAR,
                )?;
            }
            Ok(())
        }

        fn add(&self, ident: usize, filter: i16, fflags: u32, flags: u16) -> io::Result<()> {
            let change = libc::kevent {
                ident,
                filter,
                flags,
                fflags,
                data: 0,
                udata: std::ptr::null_mut(),
            };
            // SAFETY: one change in, no events out.
            check(unsafe {
                libc::kevent(
                    self.queue,
                    &change,
                    1,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null(),
                )
            })
            .map(|_| ())
        }
    }

    impl Drop for Watch {
        fn drop(&mut self) {
            for fd in self.open.drain(..).chain([self.queue]) {
                // SAFETY: descriptors this watch opened.
                unsafe { libc::close(fd) };
            }
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    /// inotify on the folder (which reports writes to the files in it too),
    /// a pidfd for the host process, and stdin, all waited on with poll.
    pub struct Watch {
        inotify: libc::c_int,
        pidfd: Option<libc::c_int>,
        process: Option<u32>,
        stdin: bool,
        pending: bool,
    }

    impl Watch {
        /// `files` live in `dir`, so the folder's watch covers them.
        pub fn new(dir: &Path, _files: &[PathBuf], stdin: bool) -> io::Result<Watch> {
            // SAFETY: the result is checked.
            let inotify = check(unsafe { libc::inotify_init1(libc::IN_CLOEXEC) })?;
            let name = CString::new(dir.as_os_str().as_bytes())?;
            let mask = libc::IN_MODIFY
                | libc::IN_CREATE
                | libc::IN_DELETE
                | libc::IN_MOVED_TO
                | libc::IN_MOVED_FROM
                | libc::IN_CLOSE_WRITE;
            // SAFETY: a NUL-terminated path; the result is checked.
            check(unsafe { libc::inotify_add_watch(inotify, name.as_ptr(), mask) })?;
            Ok(Watch {
                inotify,
                pidfd: None,
                process: None,
                stdin,
                pending: false,
            })
        }

        pub fn process(&mut self, pid: Option<u32>) {
            if pid == self.process {
                return;
            }
            self.process = pid;
            if let Some(fd) = self.pidfd.take() {
                // SAFETY: a descriptor this watch opened.
                unsafe { libc::close(fd) };
            }
            if let Some(pid) = pid {
                // SAFETY: pidfd_open takes a pid and flags; the result is checked.
                let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0) };
                if fd < 0 {
                    self.pending = true;
                } else {
                    self.pidfd = Some(fd as libc::c_int);
                }
            }
        }

        /// inotify watches the folder from `new` on; nothing to renew.
        pub fn arm(&mut self) -> io::Result<()> {
            Ok(())
        }

        pub fn wait(&mut self) -> io::Result<Woke> {
            if std::mem::take(&mut self.pending) {
                return Ok(Woke::Changed);
            }
            loop {
                let mut fds = vec![libc::pollfd {
                    fd: self.inotify,
                    events: libc::POLLIN,
                    revents: 0,
                }];
                if let Some(fd) = self.pidfd {
                    fds.push(libc::pollfd {
                        fd,
                        events: libc::POLLIN,
                        revents: 0,
                    });
                }
                if self.stdin {
                    fds.push(libc::pollfd {
                        fd: 0,
                        events: libc::POLLIN,
                        revents: 0,
                    });
                }
                // SAFETY: `fds` holds `fds.len()` entries; -1 blocks.
                let got = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, -1) };
                if got < 0 {
                    let err = io::Error::last_os_error();
                    if err.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(err);
                }
                let ready = |fd: libc::c_int| fds.iter().any(|p| p.fd == fd && p.revents != 0);
                if self.stdin && ready(0) && stdin_closed() {
                    return Ok(Woke::StdinClosed);
                }
                let mut changed = false;
                if ready(self.inotify) {
                    let mut buffer = [0u8; 4096];
                    // SAFETY: reads into `buffer`; the events are only drained.
                    unsafe { libc::read(self.inotify, buffer.as_mut_ptr().cast(), buffer.len()) };
                    changed = true;
                }
                if let Some(fd) = self.pidfd.filter(|&fd| ready(fd)) {
                    // SAFETY: a descriptor this watch opened.
                    unsafe { libc::close(fd) };
                    self.pidfd = None;
                    self.process = None;
                    changed = true;
                }
                if changed {
                    return Ok(Woke::Changed);
                }
            }
        }
    }

    impl Drop for Watch {
        fn drop(&mut self) {
            for fd in self.pidfd.take().into_iter().chain([self.inotify]) {
                // SAFETY: descriptors this watch opened.
                unsafe { libc::close(fd) };
            }
        }
    }
}

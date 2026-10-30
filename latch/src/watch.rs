//! Waiting, without polling, until something in a state folder may have
//! changed: a file in it was written, created, renamed or removed, a
//! process (the host) exited, or a descriptor given as the watch's hang-up
//! (our stdin, or a pipe's read end) closed. The kernel wakes us
//! (kqueue on macOS, inotify and a pidfd on Linux, a change notification
//! and process handle on Windows); with nothing happening the process
//! sleeps. A wake says only "look again": callers re-read what they follow
//! and compare. So nothing slips between a look and the wait, a caller arms
//! the watch, then looks, then waits.

use std::io;
#[cfg(unix)]
use std::os::fd::RawFd;
use std::path::{Path, PathBuf};

/// What a watch reads until its writer closes it: a descriptor, or on
/// Windows a handle. Whoever gives one keeps it open while the watch lives.
#[cfg(unix)]
pub type Hangup = RawFd;
#[cfg(windows)]
pub type Hangup = std::os::windows::io::RawHandle;

pub enum Woke {
    Changed,
    /// The hang-up descriptor's writer closed it: whoever started us is gone.
    HungUp,
}

/// The hang-up `reader` gives.
pub fn hangup_of(reader: &std::io::PipeReader) -> Hangup {
    #[cfg(unix)]
    return std::os::fd::AsRawFd::as_raw_fd(reader);
    #[cfg(windows)]
    return std::os::windows::io::AsRawHandle::as_raw_handle(reader);
}

/// Stdin as a hang-up, when it is a pipe.
pub fn stdin_hangup() -> Option<Hangup> {
    #[cfg(unix)]
    return stdin_is_a_pipe().then_some(libc::STDIN_FILENO);
    #[cfg(windows)]
    return stdin_is_a_pipe()
        .then(|| std::os::windows::io::AsRawHandle::as_raw_handle(&io::stdin()));
}

/// Whether stdin is a pipe or socket, so its closing means the parent went
/// away. A terminal or `/dev/null` is not watched.
#[cfg(unix)]
pub fn stdin_is_a_pipe() -> bool {
    // SAFETY: fstat needs output storage. stat contains only integer fields,
    // integer arrays and timespecs, for which all-zero bytes are valid.
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: std cannot inspect stdin's file type directly. `stat` is writable
    // for one stat value; the OS checks fd 0, and fields are used only on success.
    if unsafe { libc::fstat(0, &mut stat) } != 0 {
        return false;
    }
    let kind = stat.st_mode & libc::S_IFMT;
    kind == libc::S_IFIFO || kind == libc::S_IFSOCK
}

/// Whether stdin is a pipe, so its closing means the parent went away. A
/// console or `NUL` is not watched.
#[cfg(windows)]
pub fn stdin_is_a_pipe() -> bool {
    use windows_sys::Win32::Storage::FileSystem::{FILE_TYPE_PIPE, GetFileType};
    let stdin = std::os::windows::io::AsRawHandle::as_raw_handle(&io::stdin());
    // SAFETY: GetFileType only reads the handle's type; an invalid handle
    // answers FILE_TYPE_UNKNOWN.
    unsafe { GetFileType(stdin) == FILE_TYPE_PIPE }
}

/// Blocks until stdin is closed, dropping what it reads. On a thread of
/// its own, which never keeps the process from exiting.
pub fn wait_for_stdin_to_close() {
    let mut buffer = [0u8; 512];
    let mut stdin = std::io::stdin();
    while matches!(io::Read::read(&mut stdin, &mut buffer), Ok(read) if read > 0) {}
}

#[cfg(unix)]
fn check(result: libc::c_int) -> io::Result<libc::c_int> {
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(result)
    }
}

/// Reads and drops what `fd` has; true once it is closed.
#[cfg(unix)]
fn closed(fd: RawFd) -> bool {
    let mut buffer = [0u8; 512];
    // SAFETY: use the same unbuffered fd watched by kqueue/poll, rather than
    // std's buffered stdin. `buffer` is writable for the supplied length, and
    // the OS checks `fd`; no pointer escapes this call.
    let read = unsafe { libc::read(fd, buffer.as_mut_ptr().cast(), buffer.len()) };
    read == 0 || (read < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted)
}

#[cfg(target_os = "macos")]
pub use macos::Watch;

#[cfg(target_os = "linux")]
pub use linux::Watch;

#[cfg(windows)]
pub use windows::Watch;

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    /// A kqueue: the folder itself (an entry added, renamed or removed),
    /// each of `files` (written to), the host process, and the hang-up.
    pub struct Watch {
        queue: libc::c_int,
        dir: PathBuf,
        files: Vec<PathBuf>,
        /// Open descriptors for the folder and the files; closing one drops
        /// its events, so each wait opens them afresh.
        open: Vec<libc::c_int>,
        hangup: Option<RawFd>,
        process: Option<u32>,
        /// A change seen while arming (the process was already gone).
        pending: bool,
    }

    impl Watch {
        pub fn new(dir: &Path, files: &[PathBuf], hangup: Option<Hangup>) -> io::Result<Watch> {
            // SAFETY: std has no kqueue API. This no-argument call accesses no
            // Rust memory; check rejects errors before we store the descriptor.
            let queue = check(unsafe { libc::kqueue() })?;
            let watch = Watch {
                queue,
                dir: dir.to_path_buf(),
                files: files.to_vec(),
                open: Vec::new(),
                hangup,
                process: None,
                pending: false,
            };
            if let Some(fd) = hangup {
                watch.add(fd as usize, libc::EVFILT_READ, 0, libc::EV_ADD)?;
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
                // SAFETY: kevent needs output storage. Its integer fields may
                // be zero and its raw udata pointer may be null; neither is a reference.
                let mut event: libc::kevent = unsafe { std::mem::zeroed() };
                // SAFETY: std has no kqueue wait API. Our queue stays open,
                // `event` holds one writable output entry, and null input has
                // count 0; the null timeout blocks without dereferencing a pointer.
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
                if let Some(fd) = self.hangup
                    && event.filter == libc::EVFILT_READ
                    && event.ident == fd as usize
                {
                    if event.flags & libc::EV_EOF != 0 || closed(fd) {
                        return Ok(Woke::HungUp);
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
                // SAFETY: closing the raw event descriptor removes its watch.
                // We opened it and drained it from `open`, so it is closed only
                // once and no longer used by this watch.
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
                // SAFETY: std has no O_EVTONLY open API. `name` is a live
                // NUL-terminated path; O_CREAT is absent, so no mode argument is
                // required, and errors are checked before storing the descriptor.
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
            // SAFETY: std has no kqueue registration API. Our queue stays open,
            // `change` is live for its one input entry, and null output has count 0.
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
                // SAFETY: release this watch's raw kernel descriptors. Each was
                // opened by us, is removed from `open` or is the queue itself,
                // and is closed exactly once with no subsequent use.
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
    /// a pidfd for the host process, and the hang-up, all waited on with poll.
    pub struct Watch {
        inotify: libc::c_int,
        pidfd: Option<libc::c_int>,
        process: Option<u32>,
        hangup: Option<RawFd>,
        pending: bool,
    }

    impl Watch {
        /// `files` live in `dir`, so the folder's watch covers them.
        pub fn new(dir: &Path, _files: &[PathBuf], hangup: Option<Hangup>) -> io::Result<Watch> {
            // SAFETY: std has no inotify API. The documented CLOEXEC flag is
            // scalar, no Rust memory is accessed, and check rejects errors.
            let inotify = check(unsafe { libc::inotify_init1(libc::IN_CLOEXEC) })?;
            let name = CString::new(dir.as_os_str().as_bytes())?;
            let mask = libc::IN_MODIFY
                | libc::IN_CREATE
                | libc::IN_DELETE
                | libc::IN_MOVED_TO
                | libc::IN_MOVED_FROM
                | libc::IN_CLOSE_WRITE;
            // SAFETY: std has no inotify registration API. `inotify` is open,
            // `name` is a live NUL-terminated path, mask contains documented flags,
            // and check rejects errors before the watch is returned.
            check(unsafe { libc::inotify_add_watch(inotify, name.as_ptr(), mask) })?;
            Ok(Watch {
                inotify,
                pidfd: None,
                process: None,
                hangup,
                pending: false,
            })
        }

        pub fn process(&mut self, pid: Option<u32>) {
            if pid == self.process {
                return;
            }
            self.process = pid;
            if let Some(fd) = self.pidfd.take() {
                // SAFETY: changing the watched process requires closing the old
                // raw pidfd. take removes our owned descriptor, so it is closed
                // once and is not polled or closed again.
                unsafe { libc::close(fd) };
            }
            if let Some(pid) = pid {
                // SAFETY: std has no arbitrary-process pidfd API. This syscall
                // takes only a PID and zero flags, accesses no Rust memory, and
                // its error result is checked before storing the descriptor.
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
                if let Some(fd) = self.hangup {
                    fds.push(libc::pollfd {
                        fd,
                        events: libc::POLLIN,
                        revents: 0,
                    });
                }
                // SAFETY: std cannot wait on this set of raw event descriptors.
                // `fds` contains at most three initialized pollfd entries, is
                // writable for their count, and is not resized during the call.
                let got = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, -1) };
                if got < 0 {
                    let err = io::Error::last_os_error();
                    if err.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(err);
                }
                let ready = |fd: libc::c_int| fds.iter().any(|p| p.fd == fd && p.revents != 0);
                if let Some(fd) = self.hangup
                    && ready(fd)
                    && closed(fd)
                {
                    return Ok(Woke::HungUp);
                }
                let mut changed = false;
                if ready(self.inotify) {
                    let mut buffer = [0u8; 4096];
                    // SAFETY: drain the ready raw inotify descriptor before the
                    // next wait. It is open, `buffer` is writable for the given
                    // length, and the raw bytes are discarded without typed reads.
                    unsafe { libc::read(self.inotify, buffer.as_mut_ptr().cast(), buffer.len()) };
                    changed = true;
                }
                if let Some(fd) = self.pidfd.filter(|&fd| ready(fd)) {
                    // SAFETY: release the exited process's raw pidfd. It is
                    // still owned by this watch and is removed immediately below,
                    // so no future poll or drop uses the closed descriptor.
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
                // SAFETY: release this watch's raw kernel descriptors. take
                // removes its owned pidfd, the other descriptor is its inotify
                // instance, and neither is used or closed again after drop.
                unsafe { libc::close(fd) };
            }
        }
    }
}

#[cfg(windows)]
mod windows {
    use std::os::windows::ffi::OsStrExt;
    use std::ptr::null;

    use windows_sys::Win32::Foundation::{
        CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE, INVALID_HANDLE_VALUE,
        WAIT_FAILED, WAIT_OBJECT_0,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE,
        FILE_NOTIFY_CHANGE_SIZE, FindCloseChangeNotification, FindFirstChangeNotificationW,
        FindNextChangeNotification, ReadFile,
    };
    use windows_sys::Win32::System::Threading::{
        CreateEventW, GetCurrentProcess, INFINITE, OpenProcess, PROCESS_SYNCHRONIZE, SetEvent,
        WaitForMultipleObjects,
    };

    use super::*;

    /// A change notification on the folder (which reports writes to the
    /// files in it too), the host's process handle, and an event a thread of
    /// the watch's own sets once the hang-up closes, all waited on at once.
    pub struct Watch {
        change: HANDLE,
        process: Option<u32>,
        handle: Option<HANDLE>,
        hung_up: Option<HANDLE>,
        pending: bool,
    }

    /// `handle` again, for a thread to own and close.
    fn duplicate(handle: HANDLE) -> io::Result<HANDLE> {
        let mut copy = std::ptr::null_mut();
        // SAFETY: both process handles are this process's pseudo-handle, and
        // `copy` is writable for the duplicate, checked before use.
        let ok = unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                handle,
                GetCurrentProcess(),
                &mut copy,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(copy)
    }

    /// Sets an event once `hangup`'s writer closes it, from a thread that
    /// reads and drops what it has. The thread owns copies of both handles,
    /// so the watch may end before the writer does.
    fn on_hang_up(hangup: Hangup) -> io::Result<HANDLE> {
        // SAFETY: a manual-reset, unsignaled, unnamed event; null is failure.
        let event = unsafe { CreateEventW(null(), 1, 0, null()) };
        if event.is_null() {
            return Err(io::Error::last_os_error());
        }
        let (reader, signal) = (duplicate(hangup)? as usize, duplicate(event)? as usize);
        std::thread::spawn(move || {
            let (reader, signal) = (reader as HANDLE, signal as HANDLE);
            let mut buffer = [0u8; 512];
            let mut read = 0;
            // SAFETY: the thread owns both duplicates; `buffer` is writable
            // for its length, and each handle is closed once, here.
            unsafe {
                while ReadFile(
                    reader,
                    buffer.as_mut_ptr(),
                    buffer.len() as u32,
                    &mut read,
                    std::ptr::null_mut(),
                ) != 0
                    && read > 0
                {}
                SetEvent(signal);
                CloseHandle(reader);
                CloseHandle(signal);
            }
        });
        Ok(event)
    }

    impl Watch {
        /// `files` live in `dir`, so the folder's notification covers them.
        pub fn new(dir: &Path, _files: &[PathBuf], hangup: Option<Hangup>) -> io::Result<Watch> {
            let wide: Vec<u16> = dir.as_os_str().encode_wide().chain([0]).collect();
            let filter = FILE_NOTIFY_CHANGE_FILE_NAME
                | FILE_NOTIFY_CHANGE_DIR_NAME
                | FILE_NOTIFY_CHANGE_LAST_WRITE
                | FILE_NOTIFY_CHANGE_SIZE;
            // SAFETY: `wide` is a live NUL-terminated path; the result is
            // checked before it is stored.
            let change = unsafe { FindFirstChangeNotificationW(wide.as_ptr(), 0, filter) };
            if change == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            let mut watch = Watch {
                change,
                process: None,
                handle: None,
                hung_up: None,
                pending: false,
            };
            watch.hung_up = hangup.map(on_hang_up).transpose()?;
            Ok(watch)
        }

        pub fn process(&mut self, pid: Option<u32>) {
            if pid == self.process {
                return;
            }
            self.process = pid;
            if let Some(handle) = self.handle.take() {
                // SAFETY: the watch owned this handle; take means it is closed once.
                unsafe { CloseHandle(handle) };
            }
            if let Some(pid) = pid {
                // SAFETY: OpenProcess takes plain values; null is failure.
                let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
                if handle.is_null() {
                    self.pending = true;
                } else {
                    self.handle = Some(handle);
                }
            }
        }

        /// The notification watches the folder from `new` on, and `wait`
        /// renews it after each change.
        pub fn arm(&mut self) -> io::Result<()> {
            Ok(())
        }

        pub fn wait(&mut self) -> io::Result<Woke> {
            if std::mem::take(&mut self.pending) {
                return Ok(Woke::Changed);
            }
            // The hang-up first: when it and a change come together, it wins.
            let handles: Vec<HANDLE> = [self.hung_up, Some(self.change), self.handle]
                .into_iter()
                .flatten()
                .collect();
            // SAFETY: `handles` holds at most three live handles of the watch's.
            let woke = unsafe {
                WaitForMultipleObjects(handles.len() as u32, handles.as_ptr(), 0, INFINITE)
            };
            if woke == WAIT_FAILED {
                return Err(io::Error::last_os_error());
            }
            let which = handles[(woke - WAIT_OBJECT_0) as usize];
            if Some(which) == self.hung_up {
                return Ok(Woke::HungUp);
            }
            if which == self.change {
                // SAFETY: `change` is the watch's live notification handle.
                if unsafe { FindNextChangeNotification(self.change) } == 0 {
                    return Err(io::Error::last_os_error());
                }
            } else {
                // The host exited.
                self.process(None);
            }
            Ok(Woke::Changed)
        }
    }

    impl Drop for Watch {
        fn drop(&mut self) {
            self.process(None);
            // SAFETY: the watch owns these handles and drops them once.
            unsafe {
                FindCloseChangeNotification(self.change);
                if let Some(event) = self.hung_up {
                    CloseHandle(event);
                }
            }
        }
    }
}

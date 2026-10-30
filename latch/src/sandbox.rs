//! The worker sandbox: Seatbelt on macOS, bubblewrap and seccomp on Linux,
//! an AppContainer in a Job object on Windows (see `sandbox/windows.rs`, which
//! also says how Windows' differs).
//!
//! A worker process calls `confine` before starting any runtime or threads.
//! From then on the operating system itself enforces the policy, and it
//! cannot be lifted, not even by programs the worker starts: writes only
//! inside the writable paths and temp folder, never the host's state
//! directory or the running latch. Exec reads home and the system's files;
//! its write paths and IP access are granted per call. Host Unix sockets
//! remain inaccessible. File reads use the same home view; file writes
//! use private staging. Google workers read only their call folder and runtime files.
//! Hidden paths stay out of reach, with Library/Caches an exception for
//! workers that read home (exec, file get).
//! File and Messages workers get no network; Google workers may reach out.
//! Google and Messages workers execute only their pinned binary. None can send
//! Apple Events. The Messages worker reads its private database folders only. The
//! kernel resolves every path (symlinks included) before checking it, so a
//! link into a hidden path is refused by Seatbelt and reports `not_found`
//! in Linux's filesystem view.
//!
//! A hard link is a second name for the same file, and both systems check
//! names. A worker cannot make one to a hidden file (Seatbelt refuses it;
//! Linux shows the file empty or not at all, on a mount of its own), nor
//! rename a folder above a hidden path to move it out of the rules' way.
//! A hard link made outside latch, from a readable place to a hidden file,
//! stays readable through that name: an accepted limit.
//!
//! Linux hides the state directory with an empty, read-only mount, even when
//! an area contains it. Other systems refuse to run a worker at all.

use std::path::{Path, PathBuf};

/// What a worker may touch.
#[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
pub struct Policy {
    /// Directories the worker may read and write, with everything below them.
    pub areas: Vec<PathBuf>,
    /// Files and folders the worker may read, never write.
    pub reads: Vec<PathBuf>,
    /// Paths no worker may read or write, even inside an area or an opt-in
    /// (`never_readable`). Those that do not exist are skipped.
    pub hidden: Vec<PathBuf>,
    /// The host's state directory: denied on macOS, hidden on Linux, even in an area.
    pub state_dir: PathBuf,
    /// An `exec` worker: may start programs and has its own process view.
    pub exec: bool,
    /// Home-readable exec profile; its write paths have already been resolved.
    pub home: Option<PathBuf>,
    /// Outbound IP access; host Unix sockets are never granted.
    pub network: bool,
    /// An `exec` worker's own temp folder, made by the host for this call:
    /// readable and writable on macOS. On Linux a private TMPDIR of the
    /// worker's own takes its place, and the folder only holds bubblewrap's
    /// status file.
    pub tmpdir: Option<PathBuf>,
    /// An `exec` worker kills its program this many seconds after it
    /// started, even if the host that should have stopped it by then cannot
    /// (it hangs, or is stopped).
    pub kill_after: Option<u64>,
    /// A Google or Messages worker may run only this pinned program.
    pub program: Option<PathBuf>,
    /// An `exec` worker's folder to run in, which Windows lets it enter
    /// (only) when no grant reaches it.
    #[cfg(windows)]
    pub workdir: Option<PathBuf>,
    /// Test only: make the OS refuse the sandbox, to show that a sandbox
    /// that cannot be applied fails the operation.
    pub break_for_test: bool,
}

/// Latch's private state (`~/.latch`, and the folder latch uses without
/// `--state-dir` wherever it is) and, on macOS, Library. Exec profiles make
/// a narrow exception for Library/Caches. The host denies its own state separately.
pub fn never_readable(home: &Path) -> Vec<PathBuf> {
    std::iter::once(home.join(".latch"))
        .chain(crate::cli::default_state_dir())
        .chain(cfg!(target_os = "macos").then(|| home.join("Library")))
        .collect()
}

/// The kernel's path for an already open object, rather than another lookup
/// through a name that a concurrent command can replace with a symlink.
#[cfg(unix)]
pub(crate) fn opened_path(file: &std::fs::File) -> std::io::Result<PathBuf> {
    use std::os::fd::AsRawFd;
    #[cfg(target_os = "linux")]
    {
        std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::ffi::OsStrExt;
        let mut buffer = [0u8; libc::PATH_MAX as usize];
        // SAFETY: F_GETPATH writes at most PATH_MAX bytes to this live buffer;
        // file owns a valid descriptor and neither argument accesses Rust objects.
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETPATH, buffer.as_mut_ptr()) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let name = std::ffi::CStr::from_bytes_until_nul(&buffer).map_err(std::io::Error::other)?;
        Ok(PathBuf::from(std::ffi::OsStr::from_bytes(name.to_bytes())))
    }
}

/// The folder `path` names, opened without following a symlink at it and
/// checked to be that folder still, a folder above it swapped included: the
/// handle the host anchors a judged write to (`open_at`, `mkdirat`, …).
#[cfg(unix)]
pub(crate) fn open_folder(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let folder = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(path)?;
    if opened_path(&folder)? != path {
        return Err(std::io::Error::other(format!(
            "{} changed after it was checked",
            path.display()
        )));
    }
    Ok(folder)
}

/// `openat(folder, name, flags, mode)`, close-on-exec: `name` is looked up
/// in `folder` alone.
#[cfg(unix)]
pub(crate) fn open_at(
    folder: &std::fs::File,
    name: &std::ffi::OsStr,
    flags: i32,
    mode: libc::c_uint,
) -> std::io::Result<std::fs::File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    let name = std::ffi::CString::new(name.as_bytes()).map_err(std::io::Error::other)?;
    // SAFETY: `name` is NUL-terminated and outlives the call, and `folder`
    // holds its descriptor open.
    let fd = unsafe {
        libc::openat(
            folder.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_CLOEXEC,
            mode,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: openat just returned this descriptor, and nothing else owns it.
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

/// Confines the calling thread and any threads or processes it creates.
/// Call before starting other threads: the Linux launcher replaces this process.
/// An error means the process is not confined and must not go on.
pub fn confine(policy: &Policy) -> Result<(), String> {
    platform::confine(policy)
}

/// An `exec` worker's watch on the host that started it, taken before the
/// worker confines itself (see `supervise`).
pub use platform::HostWatch;

/// Starts watching the host that started this worker. Call before `confine`.
/// Fails if the host is already gone.
pub fn watch_host() -> Result<HostWatch, String> {
    platform::watch_host()
}

/// Before an `exec` worker is confined, makes sure
/// the program ends when the host does, however the host ends (SIGKILL
/// included), and at `kill_after` seconds at the latest, even if the host
/// that should stop it by then cannot. On macOS the worker forks; the child
/// returns to confine itself and become the program while the parent stays
/// outside its sandbox to watch, and ends as the program did. On Linux the
/// launcher outside bubblewrap does this, so this returns at once.
pub fn supervise(watch: HostWatch, kill_after: Option<u64>) -> Result<(), String> {
    platform::supervise(watch, kill_after)
}

/// Waits for our child, retrying a wait interrupted by a signal.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn reap(pid: libc::pid_t) -> std::io::Result<std::process::ExitStatus> {
    use std::os::unix::process::ExitStatusExt;
    let mut status = 0;
    loop {
        // SAFETY: std has no Child handle for this forked supervisor child.
        // `pid` is that child, and `status` is writable for one wait status.
        if unsafe { libc::waitpid(pid, &mut status, 0) } >= 0 {
            return Ok(std::process::ExitStatus::from_raw(status));
        }
        let err = std::io::Error::last_os_error();
        if err.kind() != std::io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

/// Blocks until the host `watch` watches has exited: `true`. `false` if
/// the watch broke, and there is nothing more to wait for.
#[cfg(target_os = "macos")]
pub fn wait_for_host(watch: HostWatch) -> bool {
    platform::wait_for_host(watch)
}

/// Ends this process as `status` says another ended: with the same exit
/// code, or killed by the same signal.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn exit_with_status(status: std::process::ExitStatus) -> ! {
    use std::os::unix::process::ExitStatusExt;
    if let Some(signal) = status.signal() {
        // SAFETY: std::process::exit cannot reproduce a signal termination.
        // The signal came from waitpid, SIG_DFL is a valid disposition, and
        // raise targets this process; neither call accesses Rust memory.
        unsafe {
            libc::signal(signal, libc::SIG_DFL);
            libc::raise(signal);
        }
        std::process::exit(128 + signal);
    }
    std::process::exit(status.code().unwrap_or(125));
}

/// What a Linux browser worker sees: the system files its engine and
/// display need, read-only, and of the state directory only its profile
/// (read-write) and its engine's plugin folder (read-only); its scratch
/// folder; a private `/tmp`; and the outbound network. No areas.
#[cfg(target_os = "linux")]
pub struct BrowserView {
    pub state_dir: PathBuf,
    pub profile: PathBuf,
    pub plugin: PathBuf,
    pub scratch: PathBuf,
}

/// Confines a browser worker as `confine` does other workers, to `view`.
/// The engine's own sandbox runs inside it, in namespaces of its own.
#[cfg(target_os = "linux")]
pub fn confine_browser(view: &BrowserView) -> Result<(), String> {
    platform::confine_browser(view)
}

/// Checks, without confining anything, that this system can apply the
/// worker sandbox at all. The host runs it once at startup, so a kernel
/// without the sandbox is refused there, not by every call later.
pub fn check() -> Result<(), String> {
    platform::check()
}

/// Whether this host runs Chrome without its own sandbox, inside latch's
/// worker sandbox only: where AppArmor restricts unprivileged user
/// namespaces (Ubuntu 23.10 and later) and Ubuntu's bwrap profile is in
/// force, that profile denies every program bwrap starts the capabilities
/// Chrome's sandbox needs, so Chrome would not start. Web pages there are
/// not isolated from one another, nor from the browser process; latch's
/// bubblewrap and seccomp still confine all of them. The profile is in
/// force when the restriction is on and bwrap can run at all, which is
/// what its file in /etc/apparmor.d stands for (the installer copies it
/// there once it has loaded).
pub fn page_isolation_off() -> bool {
    cfg!(target_os = "linux")
        && std::fs::read_to_string("/proc/sys/kernel/apparmor_restrict_unprivileged_userns")
            .is_ok_and(|setting| setting.trim() == "1")
        && Path::new("/etc/apparmor.d/bwrap-userns-restrict").is_file()
}

/// What the browser answers and `status --local` say where
/// `page_isolation_off`.
pub const PAGE_ISOLATION_OFF: &str = "page isolation off on this host";

pub fn name() -> &'static str {
    if cfg!(target_os = "linux") {
        "bubblewrap + seccomp"
    } else if cfg!(windows) {
        "AppContainer + Job object"
    } else {
        "Seatbelt"
    }
}

/// Preserve a launcher's diagnostic when it fails before the worker starts.
pub fn startup_error(stderr: &str) -> String {
    #[cfg(target_os = "linux")]
    {
        let cause = stderr.trim();
        let mut message = format!(
            "bubblewrap worker setup failed: {cause}. Run install.sh; check that user namespaces and mounts are permitted by the host policy."
        );
        if std::path::Path::new("/.dockerenv").exists()
            || std::path::Path::new("/run/.containerenv").exists()
        {
            message.push_str("\nDocker: use non-root --user, --cap-drop=ALL, --security-opt=no-new-privileges=true, --security-opt=seccomp=/path/to/latch/docker/seccomp.json.\nAppArmor hosts also need a loaded, validated container profile.\nDo not use --privileged or SYS_ADMIN. See 'Running a latch host in Docker' (latch/DOCKER.md).");
        }
        message
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = stderr;
        "the probe stopped without saying why".to_string()
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::{CStr, CString, c_char, c_int};
    use std::os::unix::process::ExitStatusExt;
    use std::path::{Path, PathBuf};

    use super::{Policy, exit_with_status, reap};

    // `sandbox_init` is part of macOS's system library. Apple marks it
    // deprecated but it is what `sandbox-exec` and other tools still use.
    // SAFETY: these declarations match libsystem's C ABI; their checked
    // callers keep the profile alive and release errors with sandbox_free_error.
    unsafe extern "C" {
        fn sandbox_init(profile: *const c_char, flags: u64, errorbuf: *mut *mut c_char) -> c_int;
        fn sandbox_free_error(errorbuf: *mut c_char);
    }

    /// Seatbelt is part of every macOS, so there is nothing to probe.
    pub fn check() -> Result<(), String> {
        Ok(())
    }

    /// A kqueue that reports the host's exit.
    pub struct HostWatch(c_int);

    pub fn watch_host() -> Result<HostWatch, String> {
        // SAFETY: std has no kqueue API. This no-argument call accesses no
        // Rust memory; a negative result is rejected before using the descriptor.
        let queue = unsafe { libc::kqueue() };
        if queue < 0 {
            return Err(format!("kqueue: {}", std::io::Error::last_os_error()));
        }
        let host = std::os::unix::process::parent_id() as libc::pid_t;
        // A worker whose host is gone already belongs to launchd.
        if host == 1 {
            return Err("the host that started this worker is gone".to_string());
        }
        watch_exit(queue, host)
            .map_err(|err| format!("cannot watch the host: {err}"))
            .map(|()| HostWatch(queue))
    }

    /// Adds `pid`'s exit to the events `queue` reports.
    fn watch_exit(queue: c_int, pid: libc::pid_t) -> std::io::Result<()> {
        let change = libc::kevent {
            ident: pid as usize,
            filter: libc::EVFILT_PROC,
            flags: libc::EV_ADD,
            fflags: libc::NOTE_EXIT,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        // SAFETY: std has no process-exit watch API. `queue` is our kqueue,
        // `change` stays live for its one input entry, and null output has count 0.
        let added =
            unsafe { libc::kevent(queue, &change, 1, std::ptr::null_mut(), 0, std::ptr::null()) };
        if added < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn wait_for_host(watch: HostWatch) -> bool {
        let HostWatch(queue) = watch;
        loop {
            // SAFETY: zeroed is a valid `kevent` to receive into.
            let mut event: libc::kevent = unsafe { std::mem::zeroed() };
            // SAFETY: no changes in, room for one event out, no time limit.
            let got = unsafe {
                libc::kevent(queue, std::ptr::null(), 0, &mut event, 1, std::ptr::null())
            };
            if got == 1 {
                return true;
            }
            if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                return false;
            }
        }
    }

    pub fn supervise(watch: HostWatch, kill_after: Option<u64>) -> Result<(), String> {
        // SAFETY: fork is needed to supervise outside Seatbelt. This worker
        // has not started other threads, so no other thread can leave a Rust
        // lock held in the child. Only the child returns to confine itself;
        // the parent watches and waits. kqueue descriptors are not inherited.
        let program = unsafe { libc::fork() };
        if program < 0 {
            return Err(format!("fork: {}", std::io::Error::last_os_error()));
        }
        if program == 0 {
            return Ok(());
        }
        let HostWatch(queue) = watch;
        let deadline = kill_after
            .map(|seconds| std::time::Instant::now() + std::time::Duration::from_secs(seconds));
        // A program that is already gone is reaped below all the same.
        if watch_exit(queue, program).is_ok() {
            loop {
                let left = deadline.map(|deadline| {
                    let left = deadline.saturating_duration_since(std::time::Instant::now());
                    libc::timespec {
                        tv_sec: left.as_secs() as libc::time_t,
                        tv_nsec: libc::c_long::from(left.subsec_nanos()),
                    }
                });
                // SAFETY: kevent needs output storage. Its integer fields may
                // be zero and its raw udata pointer may be null; neither is a reference.
                let mut event: libc::kevent = unsafe { std::mem::zeroed() };
                let timeout = left
                    .as_ref()
                    .map_or(std::ptr::null(), |left| left as *const _);
                // SAFETY: std has no kqueue wait API. `queue` is open, `event`
                // holds one output entry, null input has count 0, and any timeout
                // points to the live timespec in `left` for the duration of the call.
                let got =
                    unsafe { libc::kevent(queue, std::ptr::null(), 0, &mut event, 1, timeout) };
                let interrupted =
                    std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted;
                if got < 0 && interrupted {
                    continue;
                }
                if got != 1 || event.ident != program as usize {
                    // The host exited, the time is up, or the watch broke.
                    kill_all(program);
                }
                break;
            }
        }
        exit_with_status(
            reap(program).unwrap_or_else(|_| std::process::ExitStatus::from_raw(libc::SIGKILL)),
        )
    }

    /// Kills the worker's process group: the program, everything it
    /// started, and this process. The host makes each worker the leader of
    /// a group of its own; a worker run any other way kills its program alone.
    fn kill_all(program: libc::pid_t) {
        // SAFETY: std cannot kill a process group. The group is ours only when
        // we are its leader; otherwise we target only our positive fork result.
        // These scalar-only signal calls access no Rust memory.
        unsafe {
            if libc::getpgrp() == std::process::id() as libc::pid_t {
                libc::killpg(libc::getpgrp(), libc::SIGKILL);
            }
            libc::kill(program, libc::SIGKILL);
        }
    }

    pub fn confine(policy: &Policy) -> Result<(), String> {
        let profile = profile(policy)?;
        if policy.break_for_test {
            // macOS never applies a second, different profile to a process
            // that already has one, so a valid one first makes the real one
            // fail every time. (An invalid profile is no reliable failure:
            // libsandbox sometimes applies what it could parse of it.)
            apply("(version 1) (allow default)")?;
        }
        apply(&profile)
    }

    fn apply(profile: &str) -> Result<(), String> {
        let profile =
            CString::new(profile).map_err(|_| "profile contains a NUL byte".to_string())?;
        let mut error: *mut c_char = std::ptr::null_mut();
        // SAFETY: Seatbelt has no std wrapper. `profile` is a live NUL-terminated
        // CString and `error` is writable pointer storage; the API leaves it null
        // or returns an allocated diagnostic that we release below.
        let status = unsafe { sandbox_init(profile.as_ptr(), 0, &mut error) };
        if status == 0 {
            return Ok(());
        }
        let message = if error.is_null() {
            std::io::Error::last_os_error().to_string()
        } else {
            // SAFETY: reading the C diagnostic needs CStr::from_ptr. On failure,
            // this non-null error points to the library's NUL-terminated string,
            // which stays allocated until sandbox_free_error below.
            let text = unsafe { CStr::from_ptr(error) }
                .to_string_lossy()
                .into_owned();
            // SAFETY: the diagnostic must use the library's deallocator. This
            // is its original error pointer, copied into `text` and not freed yet.
            unsafe { sandbox_free_error(error) };
            text
        };
        Err(format!("the sandbox could not be applied: {message}"))
    }

    /// The Seatbelt profile (a small Lisp-like language). Later rules win
    /// over earlier ones, so the protections common to every kind come last.
    fn profile(policy: &Policy) -> Result<String, String> {
        let mut rules = if policy.exec {
            exec_rules(policy)?
        } else {
            narrow_rules(policy)?
        };
        rules.extend(protections(policy)?);
        Ok(rules.join("\n"))
    }

    /// File and Google workers: deny by default, the system files programs
    /// need to start, and the areas (file workers also read what `exec` does).
    fn narrow_rules(policy: &Policy) -> Result<Vec<String>, String> {
        let mut rules = vec![
            "(version 1)".to_string(),
            "(deny default)".to_string(),
            // What any program needs to keep running: the system's own files.
            "(allow file-read* (literal \"/\") (subpath \"/usr\") (subpath \"/System\") (subpath \"/bin\") \
             (subpath \"/private/var/db/dyld\") (literal \"/dev/null\") (literal \"/dev/urandom\"))"
                .to_string(),
            "(allow sysctl-read)".to_string(),
        ];
        // A file worker reads what `exec` does; it still writes only in the
        // areas, and the state folder stays denied.
        if policy.program.is_none() {
            rules.extend(read_rules(policy)?);
        }
        if let Some(program) = &policy.program {
            rules.push(format!(
                "(allow file-read* process-exec (literal {}))",
                quoted(program)?
            ));
            rules.push("(allow process-fork)".to_string());
            rules.push("(allow file-write-data (literal \"/dev/null\"))".to_string());
            rules.push("(deny appleevent-send)".to_string());
            rules.push("(allow file-read-metadata)".to_string());
            rules.push("(allow file-read* (subpath \"/private/etc\") (subpath \"/private/var/db/timezone\"))".to_string());
            if policy.network {
                // Apple's shared network baseline includes the system resolver
                // and notification shared memory used by getaddrinfo.
                rules.push("(import \"system.sb\") (system-network) (allow ipc-posix-shm (ipc-posix-name \"apple.shm.notification_center\"))".to_string());
                rules.push("(allow network-outbound)".to_string());
                rules.push("(allow file-read* (literal \"/private/var/run/resolv.conf\") (subpath \"/Library/Keychains\"))".to_string());
                rules.push("(allow mach-lookup (global-name \"com.apple.SystemConfiguration.configd\") (global-name \"com.apple.SystemConfiguration.DNSConfiguration\") (global-name \"com.apple.trustd.agent\") (global-name \"com.apple.trustd\"))".to_string());
            }
            // The Messages reader's databases, read-only.
            for read in &policy.reads {
                rules.push(format!("(allow file-read* (subpath {}))", quoted(read)?));
            }
        }
        // Looking a path up inside an area (as `pwd` does) needs the details
        // of each folder above it: `read_rules` grants them to a file
        // worker, the blanket metadata rule to a Google worker. A file
        // worker's temp folder is where an upload is staged.
        for area in policy.areas.iter().chain(&policy.tmpdir) {
            rules.push(format!(
                "(allow file-read* file-write* (subpath {}))",
                quoted(area)?
            ));
        }
        Ok(rules)
    }

    /// `exec` workers follow Codex CLI's macOS sandbox (vendored, see
    /// NOTICE): its base and network rules, reads as in `read_rules`, and
    /// writes only in the areas and the call's temp folder. Not its
    /// preferences rules: they read any preference through `cfprefsd`, which
    /// Codex allows only with reads of the whole filesystem. Outbound network
    /// only, no Apple Events.
    fn exec_rules(policy: &Policy) -> Result<Vec<String>, String> {
        let mut rules = vec![CODEX_BASE.to_string()];
        rules.extend(read_rules(policy)?);
        for root in policy.areas.iter().chain(&policy.tmpdir) {
            rules.push(format!(
                "(allow file-read* file-write* (subpath {}))",
                if policy.home.is_some() {
                    quoted_literal(root)?
                } else {
                    quoted(root)?
                }
            ));
        }
        if policy.network {
            rules.push("(allow network-outbound (remote ip))".to_string());
            // macOS getaddrinfo sends DNS requests through this system daemon.
            rules.push(
                "(allow network-outbound (literal \"/private/var/run/mDNSResponder\"))".to_string(),
            );
            rules.push(CODEX_NETWORK.to_string());
        }
        if let Some(tmpdir) = &policy.tmpdir {
            rules.push("(allow system-socket (socket-domain AF_UNIX))".to_string());
            rules.push(format!(
                "(allow network-outbound (remote unix-socket (subpath {})))",
                quoted(tmpdir)?
            ));
            for operation in ["network-bind", "network-inbound"] {
                rules.push(format!(
                    "(allow {operation} (local unix-socket (subpath {})))",
                    quoted(tmpdir)?
                ));
            }
        }
        rules.extend([
            // As Codex: no embedded XPC services, except the speech
            // synthesizer `say` uses, and no fcntls that change a file
            // through a read-only descriptor (F_MAKECOMPRESSED,
            // F_TRANSFEREXTENTS).
            SPEECH_XPC_EXCEPTION.to_string(),
            "(deny system-fcntl (fcntl-command 80 110))".to_string(),
            SPEECH_AND_AUDIO.to_string(),
            "(deny appleevent-send)".to_string(),
        ]);
        Ok(rules)
    }

    /// What file and `exec` workers read besides the areas and the temp
    /// folder: the system's own files and the owner's opt-ins. A file's
    /// details (as `stat` gives them) only there and on the folders above
    /// each place a worker may read, which looking a path up needs, and on
    /// `/Applications`, which resolving Xcode's tools needs: so nothing
    /// else, not even whether `~/.aws` exists, shows.
    fn read_rules(policy: &Policy) -> Result<Vec<String>, String> {
        let mut rules = vec![
            SYSTEM_READS.to_string(),
            "(allow file-read-metadata (literal \"/Applications\"))".to_string(),
        ];
        let roots = policy
            .areas
            .iter()
            .chain(&policy.tmpdir)
            .chain(&policy.reads);
        for root in roots {
            let real = root
                .canonicalize()
                .map_err(|err| format!("{}: {err}", root.display()))?;
            for above in real.ancestors().skip(1) {
                rules.push(format!(
                    "(allow file-read-metadata (literal {}))",
                    quoted(above)?
                ));
            }
        }
        for read in &policy.reads {
            rules.push(format!("(allow file-read* (subpath {}))", quoted(read)?));
        }
        Ok(rules)
    }

    /// The system's own files: what programs, their libraries, the shell,
    /// developer tools, Homebrew, name resolution and certificates need.
    /// `/Users` and the like only as folders to pass through, not to list.
    const SYSTEM_READS: &str = "(allow file-read*
  (literal \"/\") (literal \"/Users\") (literal \"/private\") (literal \"/var\")
  (literal \"/etc\") (literal \"/tmp\") (literal \"/opt\")
  (subpath \"/usr\") (subpath \"/bin\") (subpath \"/sbin\") (subpath \"/System\")
  (subpath \"/Library\") (subpath \"/private/etc\") (subpath \"/private/var/db/dyld\")
  (subpath \"/private/var/db/timezone\") (subpath \"/private/var/select\")
  (literal \"/private/var/run/resolv.conf\") (subpath \"/dev\")
  (subpath \"/opt/homebrew\") (subpath \"/Applications/Xcode.app\"))";

    /// Every embedded XPC service is denied but the speech synthesizer's.
    const SPEECH_XPC_EXCEPTION: &str = "(deny mach-lookup
  (require-all
    (xpc-service-name-prefix \"\")
    (require-not
      (xpc-service-name
        \"com.apple.ax.MauiTTSSupport.MauiAUSP\"
        \"com.apple.ax.MauiTTSSupport.MauiAUSP.apple-extension-service\"))))";

    /// What `say` needs, measured on macOS 27 with the default voice: the
    /// audio component registry, plugin lookup, the process manager and the
    /// synthesizer's service find and load the voice; `audiohald` plays it.
    /// Named services only, no blanket `mach-lookup`. Speech still needs a
    /// host started in the logged-in (Aqua) session: from a background
    /// session macOS synthesizes silence under any sandbox.
    const SPEECH_AND_AUDIO: &str = "(allow mach-lookup
  (global-name \"com.apple.audio.AudioComponentRegistrar\")
  (global-name \"com.apple.pluginkit.pkd\")
  (global-name \"com.apple.runningboard\")
  (global-name \"com.apple.audio.audiohald\")
  (xpc-service-name \"com.apple.ax.MauiTTSSupport.MauiAUSP\")
  (xpc-service-name \"com.apple.ax.MauiTTSSupport.MauiAUSP.apple-extension-service\"))";

    const CODEX_BASE: &str = include_str!("sandbox/codex/seatbelt_base_policy.sbpl");
    const CODEX_NETWORK: &str = include_str!("sandbox/codex/seatbelt_network_policy.sbpl");

    /// For every kind, last: the state directory and the hidden paths can
    /// be neither read nor written, their sockets never connected to, and
    /// the running latch never written.
    /// Seatbelt matches paths, so renaming a folder above any of them would
    /// move it out of reach of these rules; directories above them inside a
    /// writable folder, and the writable folders themselves, cannot be
    /// renamed or removed.
    fn protections(policy: &Policy) -> Result<Vec<String>, String> {
        let real = |path: &Path| {
            path.canonicalize()
                .map_err(|err| format!("{}: {err}", path.display()))
        };
        let state = real(&policy.state_dir)?;
        let latch = real(&std::env::current_exe().map_err(|err| err.to_string())?)?;
        let mut writable = Vec::new();
        for root in policy.areas.iter().chain(&policy.tmpdir) {
            writable.push(if policy.home.is_some() {
                root.clone()
            } else {
                real(root)?
            });
        }
        let mut rules = vec![
            // Not even run: a plugin there is its own worker's alone.
            format!(
                "(deny file-read* file-write* process-exec (subpath {}))",
                quoted(&state)?
            ),
            // Socket connects are network operations, not file reads or writes.
            format!(
                "(deny network-outbound (remote unix-socket (subpath {})))",
                quoted(&state)?
            ),
            format!("(deny file-write* (literal {}))", quoted(&latch)?),
        ];
        let mut hidden = Vec::new();
        for path in policy.hidden.iter().filter(|path| path.exists()) {
            let path = real(path)?;
            let filter = if policy
                .home
                .as_ref()
                .is_some_and(|home| path == home.join("Library"))
            {
                format!(
                    "(require-all (subpath {}) (require-not (subpath {})))",
                    quoted(&path)?,
                    quoted_literal(&path.join("Caches"))?
                )
            } else {
                format!("(subpath {})", quoted(&path)?)
            };
            rules.push(format!("(deny file-read* file-write* {filter})"));
            rules.push(format!(
                "(deny network-outbound (remote unix-socket {filter}))"
            ));
            hidden.push(path);
        }
        // The one part of the state folder a worker may see: a Google
        // worker reads, and runs, its own plugin's folder (gog's version
        // folder), never writes it.
        if let Some(program) = &policy.program {
            let program = real(program)?;
            let folder = program.parent().unwrap_or(&program);
            rules.push(format!("(allow file-read* (subpath {}))", quoted(folder)?));
            rules.push(format!(
                "(allow process-exec (literal {}))",
                quoted(&program)?
            ));
        }
        let mut anchors: Vec<&Path> = writable.iter().map(PathBuf::as_path).collect();
        for protected in [&state, &latch].into_iter().chain(&hidden) {
            for above in protected.ancestors().skip(1) {
                if writable.iter().any(|root| above.starts_with(root)) {
                    anchors.push(above);
                }
            }
        }
        anchors.sort();
        anchors.dedup();
        for dir in anchors {
            rules.push(format!(
                "(deny file-write-unlink (require-all (vnode-type DIRECTORY) (literal {})))",
                if policy.home.is_some() {
                    quoted_literal(dir)?
                } else {
                    quoted(dir)?
                }
            ));
        }
        Ok(rules)
    }

    /// A directory as a profile string. Seatbelt compares real paths (on
    /// macOS `/tmp` is really `/private/tmp`), so the path is resolved first.
    fn quoted(path: &Path) -> Result<String, String> {
        let real = path
            .canonicalize()
            .map_err(|err| format!("{}: {err}", path.display()))?;
        quoted_literal(&real)
    }

    /// Keep judged grants literal: resolving them again could grant a new
    /// symlink target. The kernel resolves access paths when enforcing them.
    fn quoted_literal(path: &Path) -> Result<String, String> {
        let text = path
            .to_str()
            .ok_or_else(|| format!("{} is not valid UTF-8", path.display()))?;
        // The profile language quotes like JSON: escape `\` and `"`.
        Ok(format!(
            "\"{}\"",
            text.replace('\\', "\\\\").replace('"', "\\\"")
        ))
    }
}

#[cfg(target_os = "linux")]
#[path = "sandbox/linux.rs"]
mod platform;

#[cfg(windows)]
#[path = "sandbox/windows.rs"]
mod platform;

#[cfg(windows)]
pub(crate) use platform::final_path;
#[cfg(windows)]
pub use platform::{list_folders, sweep, tie_to_host, volume_roots};

/// Whether agents may list folders here: only Windows asks
/// (`latch volume-roots`), so nothing to say elsewhere.
#[cfg(not(windows))]
pub fn list_folders() -> Option<&'static str> {
    None
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
mod platform {
    use super::Policy;

    pub fn check() -> Result<(), String> {
        Err("no worker sandbox on this operating system yet".to_string())
    }

    pub fn confine(_policy: &Policy) -> Result<(), String> {
        Err("no worker sandbox on this operating system yet".to_string())
    }

    pub struct HostWatch;

    pub fn watch_host() -> Result<HostWatch, String> {
        Err("no worker sandbox on this operating system yet".to_string())
    }

    pub fn supervise(_watch: HostWatch, _kill_after: Option<u64>) -> Result<(), String> {
        Err("no worker sandbox on this operating system yet".to_string())
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod google_tests {
    use super::*;
    use std::{fs, process::Command};
    #[test]
    fn each_google_worker_can_read_only_its_private_call() {
        if let Ok(root) = std::env::var("LATCH_PRIVATE_CALL_TEST") {
            let root = PathBuf::from(root);
            let call = root.join(std::env::var("LATCH_PRIVATE_CALL_NAME").unwrap());
            let inside_state = call.starts_with(root.join("state"));
            let confined = confine(&Policy {
                areas: vec![call.clone()],
                reads: vec![],
                hidden: vec![],
                state_dir: root.join("state"),
                exec: false,
                home: None,
                network: true,
                tmpdir: None,
                kill_after: None,
                program: Some(PathBuf::from(
                    std::env::var_os("LATCH_PRIVATE_PROGRAM").unwrap(),
                )),
                break_for_test: false,
            });
            if inside_state {
                if confined.is_ok() {
                    assert!(
                        fs::read(call.join("input")).is_err(),
                        "state must stay denied even if named as the Google area"
                    );
                }
                return;
            }
            confined.unwrap();
            assert_eq!(fs::read(call.join("input")).unwrap(), b"own input");
            for path in [
                "state/secret",
                "state/call/input",
                "private/other-call/input",
                "area/inside",
            ] {
                assert!(fs::read(root.join(path)).is_err(), "{path} must be private");
            }
            return;
        }
        let parent = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.dev/tests");
        fs::create_dir_all(&parent).unwrap();
        let dir = tempfile::tempdir_in(parent).unwrap();
        for name in ["area", "state/call", "private/call", "private/other-call"] {
            fs::create_dir_all(dir.path().join(name)).unwrap();
        }
        for (name, data) in [
            ("area/inside", "configured area"),
            ("state/secret", "secret"),
            ("private/call/input", "own input"),
            ("state/call/input", "state input"),
            ("private/other-call/input", "other input"),
        ] {
            fs::write(dir.path().join(name), data).unwrap();
        }
        for call in ["private/call", "state/call"] {
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "sandbox::google_tests::each_google_worker_can_read_only_its_private_call",
                    "--nocapture",
                ])
                .env(
                    "LATCH_PRIVATE_CALL_TEST",
                    dir.path().canonicalize().unwrap(),
                )
                .current_dir(dir.path().join(call))
                .env("LATCH_PRIVATE_CALL_NAME", call)
                .env("LATCH_PRIVATE_PROGRAM", std::env::current_exe().unwrap())
                .output()
                .unwrap();
            if cfg!(target_os = "linux") && call == "state/call" {
                // Hiding state also hides this deliberately invalid working directory.
                assert!(!output.status.success(), "{output:?}");
            } else {
                assert!(output.status.success(), "{call}: {output:?}");
            }
        }
    }

    #[test]
    fn only_a_google_worker_sees_its_own_plugin_folder() {
        if let Ok(root) = std::env::var("LATCH_PLUGIN_TEST") {
            let root = PathBuf::from(root);
            let mode = std::env::var("LATCH_PLUGIN_MODE").unwrap();
            let gog = root.join("state/plugins/gog/0.1/gog");
            confine(&Policy {
                areas: vec![root.join("private/call")],
                reads: vec![],
                hidden: vec![],
                state_dir: root.join("state"),
                exec: mode == "exec",
                home: None,
                network: mode == "exec" || mode == "google",
                tmpdir: (mode == "exec").then(|| root.join("private/call")),
                kill_after: None,
                program: (mode == "google").then(|| gog.clone()),
                break_for_test: false,
            })
            .unwrap();
            assert_eq!(
                fs::read(&gog).is_ok(),
                mode == "google",
                "{mode} reading gog"
            );
            // Only the Google worker may run it: exec and file workers may
            // not even start a program from the state folder.
            let ran = Command::new(&gog).arg("--list").output();
            assert_eq!(
                ran.is_ok_and(|out| out.status.success()),
                mode == "google",
                "{mode} running gog"
            );
            for other in ["state/secret", "state/plugins/other/1/tool"] {
                assert!(
                    fs::read(root.join(other)).is_err(),
                    "{mode} reading {other}"
                );
            }
            // Read-only, even for its own worker.
            assert!(fs::write(root.join("state/plugins/gog/0.1/new"), b"x").is_err());
            return;
        }
        let parent = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.dev/tests");
        fs::create_dir_all(&parent).unwrap();
        let dir = tempfile::tempdir_in(parent).unwrap();
        for name in [
            "private/call",
            "state/plugins/gog/0.1",
            "state/plugins/other/1",
        ] {
            fs::create_dir_all(dir.path().join(name)).unwrap();
        }
        fs::write(dir.path().join("state/secret"), "secret").unwrap();
        fs::write(dir.path().join("state/plugins/other/1/tool"), "other").unwrap();
        fs::copy(
            std::env::current_exe().unwrap(),
            dir.path().join("state/plugins/gog/0.1/gog"),
        )
        .unwrap();
        for mode in ["google", "exec", "file"] {
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "sandbox::google_tests::only_a_google_worker_sees_its_own_plugin_folder",
                    "--nocapture",
                ])
                .env("LATCH_PLUGIN_TEST", dir.path().canonicalize().unwrap())
                .env("LATCH_PLUGIN_MODE", mode)
                .current_dir(dir.path().join("private/call"))
                .output()
                .unwrap();
            assert!(output.status.success(), "{mode}: {output:?}");
        }
    }
}

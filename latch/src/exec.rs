//! The host's side of `exec`: run one program in a sandboxed worker
//! (`worker.rs`) and report how it ended and what it printed.
//!
//! The program gets an argv list, never a shell line, and an empty stdin
//! unless the client sent some (`--stdin`, at most 16 KiB of UTF-8 text).
//! A task writes it to the program and then closes the pipe. Each output stream is
//! kept up to 256 KiB (the rest is read and dropped, and `truncated` is
//! set), so a runaway command cannot flood the agent. At the time limit the
//! whole process group (the program and everything it started) is killed.
//! So is it, cut off, when the host stops (`interrupted`) or the caller's
//! connection closes (`caller_gone`): nobody would get its result. A host
//! that dies without stopping takes the program with it (see
//! `WorkerSetup::start_exec`).
//! The audit record holds the whole argv, as run, the workdir and how it
//! ended, never the output. The audit log is encrypted; whatever shows the
//! argv to people hides credential-looking values first (`redact.rs`).
//!
//! Each run gets a fresh, empty temp folder of its own as its `TMPDIR`,
//! made in the host's OS temp folder and readable and writable by that
//! worker only (on Linux the worker's private `/tmp` plays that part), and
//! the folder is deleted when the run ends. It never lands in a shared
//! folder, which may not be writable (a shared `/`, say). A host that was
//! killed leaves its folders behind; the next host to start removes them.

use std::collections::BTreeMap;
use std::future::Future;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
#[cfg(windows)]
use std::os::windows::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Child;
use tokio::task::JoinHandle;

use crate::failure::{Code, Failure, code};
use crate::log::log;
use crate::serve::Done;
use crate::state::{OwnerOnly, to_hex};
use crate::wire::Response;
use crate::worker::{WorkerSetup, authenticated_worker_error};

/// How much of each output stream the agent gets back.
const OUTPUT_CAP: usize = 256 * 1024;
/// The longest `--timeout` a host accepts.
const MAX_TIMEOUT_SECS: u64 = 600;
/// After the program ends, how long to wait for its output pipes to close
/// (a process that left the group could otherwise hold them open).
const DRAIN_GRACE: Duration = Duration::from_secs(5);
/// How long after its timeout the worker kills a program itself, if the
/// host has not by then.
const WORKER_GRACE: u64 = 5;

#[derive(Deserialize, Serialize)]
pub(crate) struct ExecArgs {
    pub argv: Vec<String>,
    #[serde(default = "default_timeout")]
    timeout: u64,
    pub workdir: Option<String>,
    stdin: Option<Vec<u8>>,
    #[serde(default)]
    pub writes: Vec<PathBuf>,
    #[serde(default)]
    pub network: bool,
    #[serde(default)]
    pub secret_env: BTreeMap<String, String>,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct PrivateExec {
    pub secret_env: BTreeMap<String, String>,
    pub error_token: String,
}

/// A program that ran past its `--timeout`: an error, carrying everything
/// the host answered (the output it wrote before it was stopped included)
/// in its details.
pub fn timed_out(timeout: u64, result: Value) -> Failure {
    let message = format!("the program ran past its {timeout} s timeout and was stopped");
    let mut failure = Failure::new(code::timed_out, message);
    failure.details = result;
    failure
}

/// The `exec` operation's arguments for `latch exec`, its stdin read now
/// when `-i` sends it.
pub fn request(args: &crate::cli::ExecArgs) -> Result<Value, Failure> {
    let secret_env = parse_secret_env(&args.secret_env)?;
    let mut request = json!({ "argv": args.argv, "workdir": args.workdir, "timeout": args.timeout,
                "writes": args.writes, "network": args.network, "secret_env": secret_env });
    if args.stdin {
        let bytes =
            crate::input::read_all(Duration::from_secs(args.timeout), crate::gate::JUDGED_STDIN)?;
        request["stdin"] = json!(bytes);
        crate::gate::judged_stdin(&request["stdin"])?;
    }
    Ok(request)
}

pub fn parse_secret_env(values: &[String]) -> Result<BTreeMap<String, String>, Failure> {
    let mut secrets = BTreeMap::new();
    for value in values {
        let (name, value) = value.split_once('=').ok_or_else(bad_secret_env)?;
        if secrets
            .insert(name.to_string(), value.to_string())
            .is_some()
        {
            return Err(bad_secret_env());
        }
    }
    validate_secret_env(&secrets)?;
    Ok(secrets)
}

fn bad_secret_env() -> Failure {
    Failure::new(code::bad_secret_env, "use distinct NAME=VALUE entries with valid environment names; HOME, PATH, TMPDIR, LATCH_*, LD_* and DYLD_* are reserved".into())
}

pub(crate) fn validate_secret_env(secrets: &BTreeMap<String, String>) -> Result<(), Failure> {
    for (name, value) in secrets {
        let mut bytes = name.bytes();
        let valid = bytes
            .next()
            .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
            && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_');
        if !valid
            || ["HOME", "PATH", "TMPDIR"].contains(&name.as_str())
            || ["LATCH_", "LD_", "DYLD_"]
                .iter()
                .any(|prefix| name.starts_with(prefix))
            || value.contains('\0')
        {
            return Err(bad_secret_env());
        }
    }
    Ok(())
}

pub(crate) fn secret_metadata(secrets: &BTreeMap<String, String>) -> Value {
    Value::Array(
        secrets
            .iter()
            .map(|(name, value)| json!({"name": name, "secret": true, "length": value.len()}))
            .collect(),
    )
}

/// A validated request whose resolved paths are the ones the judge sees.
#[derive(Serialize)]
#[serde(transparent)]
pub struct PreparedExec(ExecArgs);

fn default_timeout() -> u64 {
    60
}

/// Resolves host path fields before judgment, without creating anything.
pub fn normalize(workers: &WorkerSetup, value: &Value) -> Result<PreparedExec, Failure> {
    let bad = |message: &str| Failure::new(code::bad_request, message.to_string());
    if !value.is_object() {
        return Err(bad("exec arguments must be an object"));
    }
    let mut args: ExecArgs = serde_json::from_value(value.clone())
        .map_err(|_| bad("exec arguments have invalid types"))?;
    validate_secret_env(&args.secret_env)?;
    if !argv_is_usable(&args.argv) {
        return Err(bad("argv must be a non-empty list of strings"));
    }
    if !(1..=MAX_TIMEOUT_SECS).contains(&args.timeout) {
        return Err(bad("timeout must be 1 to 600 seconds"));
    }
    for path in &mut args.writes {
        *path = resolve(&workers.home, path)?;
        check_path(workers, path, true)?;
        if !path.exists() && !path.parent().is_some_and(Path::is_dir) {
            return Err(bad(
                "a missing write path must have an existing parent folder",
            ));
        }
    }
    let workdir = match args.workdir.as_deref() {
        Some(path) => resolve(&workers.home, Path::new(path))?,
        None => args
            .writes
            .first()
            .filter(|path| !path.is_file())
            .cloned()
            .unwrap_or_else(|| workers.home.clone()),
    };
    check_path(workers, &workdir, false)?;
    if !workdir.is_dir() && (workdir.exists() || !args.writes.contains(&workdir)) {
        return Err(bad(
            "workdir must be an existing folder or a declared new write folder",
        ));
    }
    args.workdir = Some(
        workdir
            .to_str()
            .ok_or_else(|| bad("host paths must be valid UTF-8"))?
            .to_owned(),
    );
    if args.writes.iter().any(|path| path.to_str().is_none()) {
        return Err(bad("host paths must be valid UTF-8"));
    }
    Ok(PreparedExec(args))
}

/// A resolved `path` as a person reads it, and as the judge, the audit and
/// the caller are shown it. On Windows a resolved path is verbatim
/// (`\\?\C:\x`); shown, it is `C:\x`, and `\\?\UNC\server\share` is
/// `\\server\share`, unless the plain form would name something else (a
/// part ending in a dot or a space, or a device such as `NUL`), which is shown
/// as it is. latch itself goes on using the resolved path.
pub(crate) fn shown(path: &str) -> String {
    #[cfg(windows)]
    {
        let plain = match path.strip_prefix(r"\\?\UNC\") {
            Some(share) => Some(format!(r"\\{share}")),
            None => path
                .strip_prefix(r"\\?\")
                .filter(|rest| rest.as_bytes().get(1) == Some(&b':'))
                .map(str::to_owned),
        };
        const DEVICES: &[&str] = &[
            "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
            "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
        ];
        let same = |plain: &str| {
            plain.split('\\').skip(1).all(|part| {
                let stem = part.split('.').next().unwrap_or(part).trim_end();
                !part.ends_with(['.', ' ']) && !DEVICES.iter().any(|d| d.eq_ignore_ascii_case(stem))
            })
        };
        if let Some(plain) = plain.filter(|plain| same(plain)) {
            return plain;
        }
    }
    path.to_owned()
}

pub(crate) fn resolve(home: &Path, path: &Path) -> Result<PathBuf, Failure> {
    let path = match path.strip_prefix("~") {
        Ok(rest) => home.join(rest),
        Err(_) if path.is_relative() => home.join(path),
        Err(_) => path.to_path_buf(),
    };
    // A Windows path can name another machine (`\\server\share`): looking it
    // up signs this user in there. Only this PC's drives are looked at.
    #[cfg(windows)]
    {
        use std::path::{Component, Prefix};
        let local = matches!(path.components().next(), Some(Component::Prefix(prefix))
            if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)));
        if !local {
            let message = "the host path is not on one of this PC's drives".into();
            return Err(Failure::new(code::bad_path, message));
        }
    }
    let mut above = path.as_path();
    let mut missing = Vec::new();
    loop {
        match above.canonicalize() {
            Ok(mut real) => {
                for name in missing.iter().rev() {
                    real.push(name);
                }
                return Ok(real);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let Some(name) = above.file_name() else { break };
                missing.push(name.to_os_string());
                let Some(parent) = above.parent() else { break };
                above = parent;
            }
            Err(_) => break,
        }
    }
    Err(Failure::new(
        code::bad_path,
        "the host path cannot be resolved".into(),
    ))
}

/// Whether `path` (resolved) is latch state or a hidden path, where no
/// worker reads or writes.
pub(crate) fn is_hidden(workers: &WorkerSetup, path: &Path) -> bool {
    path.starts_with(&workers.state_dir)
        || workers.hidden.iter().any(|hidden| {
            let hidden = hidden.canonicalize().unwrap_or_else(|_| hidden.clone());
            let cache = cfg!(target_os = "macos")
                && hidden == workers.home.join("Library")
                && path.starts_with(workers.home.join("Library/Caches"));
            path.starts_with(hidden) && !cache
        })
}

pub(crate) fn check_path(workers: &WorkerSetup, path: &Path, write: bool) -> Result<(), Failure> {
    let hidden = is_hidden(workers, path);
    if !path.starts_with(&workers.home) || (write && path == workers.home) || hidden {
        return Err(Failure::new(code::bad_path,
            "the path must be inside the host's home and outside hidden state; home itself cannot be a write target".into()));
    }
    // A deny entry doesn't bind a Windows container (`sandbox/windows.rs`):
    // the latch the host runs stays out of a worker's reach only while no
    // grant reaches it. So no folder holding it is written, nor anything in
    // its folder, where a program it loads could be put.
    #[cfg(windows)]
    if write
        && let Some(folder) = workers
            .program
            .canonicalize()
            .ok()
            .as_deref()
            .and_then(Path::parent)
        && (crate::sandbox::within(folder, path) || crate::sandbox::within(path, folder))
    {
        return Err(Failure::new(
            code::bad_path,
            format!(
                "the latch this host runs is in {}: a write path may not hold that folder or be inside it",
                folder.display()
            ),
        ));
    }
    // Nor a folder holding latch's state or a hidden path: a worker that may
    // delete it could rename a folder above them and put another state in
    // its place.
    #[cfg(windows)]
    if write
        && let Some(kept) = std::iter::once(&workers.state_dir)
            .chain(&workers.hidden)
            .find(|kept| crate::sandbox::within(kept, path))
    {
        return Err(Failure::new(
            code::bad_path,
            format!(
                "{} holds {}, latch's state or a hidden path: a write path may not hold one",
                path.display(),
                kept.display()
            ),
        ));
    }
    Ok(())
}

/// Only existing, plain cache directories receive implicit write access.
pub(crate) fn caches(workers: &WorkerSetup) -> Vec<PathBuf> {
    // On Windows a grant rewrites the access list of everything below it,
    // which for a large cache would slow every call: none yet.
    if cfg!(windows) {
        return Vec::new();
    }
    [
        ".cache",
        ".npm",
        ".cargo/registry",
        ".cargo/git",
        "go/pkg/mod",
    ]
    .into_iter()
    .chain(cfg!(target_os = "macos").then_some("Library/Caches"))
    .map(|name| workers.home.join(name))
    .filter(|path| {
        check_path(workers, path, true).is_ok()
            && path
                .ancestors()
                .take_while(|p| *p != workers.home)
                .all(|p| std::fs::symlink_metadata(p).is_ok_and(|meta| meta.is_dir()))
    })
    .collect()
}

/// `exec`: runs `args.argv` and returns its result. If `cut_off` finishes
/// first, the program is killed, and its outcome is the one `cut_off` gives.
pub async fn run(
    workers: &WorkerSetup,
    prepared: PreparedExec,
    cut_off: impl Future<Output = Code>,
) -> Done {
    let mut args = prepared.0;
    let timeout = args.timeout;
    // What the program reads on stdin: the bytes the client sent, or none.
    let input = args.stdin.take();
    let writes: Vec<String> = args
        .writes
        .iter()
        .map(|path| shown(&path.to_string_lossy()))
        .collect();
    let mut audit = json!({ "argv": args.argv, "workdir": args.workdir.as_deref().map(shown),
                           "writes": writes, "network": args.network,
                           "secret_env": secret_metadata(&args.secret_env),
                           "timeout": timeout });
    if let Some(bytes) = &input {
        audit["stdin"] = stdin_audit(bytes);
    }

    for path in args
        .writes
        .iter()
        .map(PathBuf::as_path)
        .chain(args.workdir.as_deref().map(Path::new))
    {
        if !resolve(&workers.home, path).is_ok_and(|real| real == path) {
            return Done::failed(
                code::bad_path,
                "the path changed after judgment".into(),
                audit,
            );
        }
    }

    for path in &args.writes {
        if !path.exists()
            && let Err(error) = create_write_dir(path)
        {
            return Done::failed(code::write_path_failed, error.to_string(), audit);
        }
    }

    let tmpdir = match make_tmpdir() {
        Ok(tmpdir) => tmpdir,
        Err(err) => {
            let message = format!("{}: {err}", std::env::temp_dir().display());
            return Done::failed(code::tmpdir_failed, message, audit);
        }
    };
    let limits = Limits { timeout, cut_off };
    let done = run_program(workers, &args, &tmpdir, input, limits, audit).await;
    // `remove_dir_all` deletes links the program left there, never what
    // they point to.
    if let Err(err) = std::fs::remove_dir_all(&tmpdir) {
        log(&format!(
            "exec: could not remove {}: {err}",
            tmpdir.display()
        ));
    }
    done
}

/// Windows' `mkdirat`: the judged parent opened as itself (a link at it
/// refused) and checked to be the folder that was judged, held open with no
/// delete sharing, and the new folder made in it by its name alone
/// (`NtCreateFile` relative to that handle). A folder swapped for a junction
/// after judging, by a worker an area lets delete it, can't redirect the
/// host's make into another place, latch's state among them.
#[cfg(windows)]
fn create_write_dir(path: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
    use windows_sys::Wdk::Storage::FileSystem::{
        FILE_CREATE, FILE_DIRECTORY_FILE, FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT,
        NtCreateFile,
    };
    use windows_sys::Win32::Foundation::{CloseHandle, RtlNtStatusToDosError, UNICODE_STRING};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ADD_SUBDIRECTORY, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_READ,
        FILE_SHARE_WRITE, SYNCHRONIZE,
    };
    use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(std::io::Error::other("no parent folder"));
    };
    let folder = std::fs::OpenOptions::new()
        .access_mode(
            FILE_LIST_DIRECTORY | FILE_ADD_SUBDIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
        )
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(parent)?;
    let real = crate::sandbox::final_path(&folder)?;
    let same = crate::sandbox::within(&real, parent) && crate::sandbox::within(parent, &real);
    if folder.metadata()?.file_type().is_symlink() || !same {
        return Err(std::io::Error::other(format!(
            "{} changed after it was checked",
            parent.display()
        )));
    }
    let mut wide: Vec<u16> = name.encode_wide().collect();
    let bytes = (wide.len() * 2) as u16;
    let name = UNICODE_STRING {
        Length: bytes,
        MaximumLength: bytes,
        Buffer: wide.as_mut_ptr(),
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: folder.as_raw_handle(),
        ObjectName: &name,
        // OBJ_CASE_INSENSITIVE, as Win32 names are.
        Attributes: 0x40,
        SecurityDescriptor: std::ptr::null(),
        SecurityQualityOfService: std::ptr::null(),
    };
    let mut status = IO_STATUS_BLOCK::default();
    let mut made = std::ptr::null_mut();
    // SAFETY: a live folder handle and name, `made` and `status` writable;
    // the new folder's handle is closed below.
    let result = unsafe {
        NtCreateFile(
            &mut made,
            FILE_LIST_DIRECTORY | SYNCHRONIZE,
            &attributes,
            &mut status,
            std::ptr::null(),
            FILE_ATTRIBUTE_NORMAL,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            FILE_CREATE,
            FILE_DIRECTORY_FILE | FILE_SYNCHRONOUS_IO_NONALERT | FILE_OPEN_REPARSE_POINT,
            std::ptr::null(),
            0,
        )
    };
    if result < 0 {
        // SAFETY: converts a status code.
        return Err(std::io::Error::from_raw_os_error(
            unsafe { RtlNtStatusToDosError(result) } as i32,
        ));
    }
    // SAFETY: the new folder's handle, closed once.
    unsafe { CloseHandle(made) };
    Ok(())
}

#[cfg(unix)]
fn create_write_dir(path: &Path) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("no parent folder"))?;
    let folder = crate::sandbox::open_folder(parent)?;
    let name = std::ffi::CString::new(
        path.file_name()
            .ok_or_else(|| std::io::Error::other("no folder name"))?
            .as_bytes(),
    )
    .map_err(std::io::Error::other)?;
    // SAFETY: the file owns a live directory descriptor and name is one NUL-
    // terminated component. Anchoring mkdirat there cannot follow a swapped parent.
    if unsafe { libc::mkdirat(folder.as_raw_fd(), name.as_ptr(), 0o777) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// What the audit keeps of an exec's stdin: its size and sha256, never
/// its content.
pub fn stdin_audit(bytes: &[u8]) -> Value {
    json!({ "size": bytes.len(), "sha256": to_hex(&Sha256::digest(bytes)) })
}

/// A new, empty folder `latch-exec-<host pid>-<random>` in the OS temp
/// folder, for this user only. Its real path, since the sandbox matches
/// real paths (macOS's temp folder is behind the `/var` link).
fn make_tmpdir() -> std::io::Result<PathBuf> {
    let name = format!(
        "latch-exec-{}-{:016x}",
        std::process::id(),
        rand::random::<u64>()
    );
    let path = std::env::temp_dir().join(name);
    // `create` (not `recursive`) fails if the name already exists.
    std::fs::DirBuilder::new().owner_only().create(&path)?;
    let real = path.canonicalize()?;
    // On Windows the temp folder may be named with 8.3 short names
    // (C:\Users\RUNNER~1\...), which the sandbox's check that it grants what
    // was judged would not match: its real, long path, without the `\\?\`
    // that programs given it as TMP may not read.
    #[cfg(windows)]
    if let Some(plain) = real.to_str().and_then(|text| text.strip_prefix(r"\\?\"))
        && plain.as_bytes().get(1) == Some(&b':')
    {
        return Ok(PathBuf::from(plain));
    }
    Ok(real)
}

/// Removes the temp folders that killed hosts left: this user's
/// `latch-exec-<pid>-…` folders in the OS temp folder whose host no longer
/// runs. Those of hosts still running (this one, or another on another
/// state folder) stay.
pub fn sweep_tmpdirs() {
    let temp = std::env::temp_dir();
    let Ok(entries) = std::fs::read_dir(&temp) else {
        return;
    };
    // SAFETY: std has no UID API. getuid takes no arguments, accesses no Rust
    // memory, and returns our real UID for the ownership check below.
    #[cfg(unix)]
    let me = unsafe { libc::getuid() };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let host = name
            .to_str()
            .and_then(|name| name.strip_prefix("latch-exec-"))
            .and_then(|rest| rest.split_once('-'))
            .and_then(|(pid, _)| pid.parse::<u32>().ok());
        let Some(host) = host.filter(|pid| *pid > 0) else {
            continue;
        };
        let Ok(meta) = entry.path().symlink_metadata() else {
            continue;
        };
        #[cfg(unix)]
        let theirs = meta.uid() != me;
        #[cfg(windows)]
        let theirs = false;
        if !meta.is_dir() || theirs || running(host) {
            continue;
        }
        match std::fs::remove_dir_all(entry.path()) {
            Ok(()) => log(&format!(
                "exec: removed {}, left by a host that was killed",
                entry.path().display()
            )),
            Err(err) => log(&format!(
                "exec: could not remove {}: {err}",
                entry.path().display()
            )),
        }
    }
}

/// Whether a process `pid` runs (one of another user counts too).
fn running(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // SAFETY: std cannot probe an arbitrary PID. Signal 0 checks existence
        // without delivering a signal; this scalar-only call accesses no Rust memory.
        let found = unsafe { libc::kill(pid as libc::pid_t, 0) } == 0;
        found || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
        };
        // SAFETY: OpenProcess takes plain values; null is failure.
        let process = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        if process.is_null() {
            return false;
        }
        // SAFETY: a live handle of ours, closed once.
        let alive = unsafe { WaitForSingleObject(process, 0) } == WAIT_TIMEOUT;
        // SAFETY: closed once, here.
        unsafe { CloseHandle(process) };
        alive
    }
}

/// When a program must stop: at `timeout` seconds, or when `cut_off`
/// finishes, with the outcome it gives.
struct Limits<F> {
    timeout: u64,
    cut_off: F,
}

/// How the wait for a program ended.
enum End {
    Exited,
    TimedOut,
    /// Cut off, with this outcome.
    Cut(Code),
}

/// Starts the program and waits for it, within `limits`.
async fn run_program(
    workers: &WorkerSetup,
    args: &ExecArgs,
    tmpdir: &Path,
    input: Option<Vec<u8>>,
    limits: Limits<impl Future<Output = Code>>,
    mut audit: Value,
) -> Done {
    let started = Instant::now();
    let kill_after = limits.timeout + WORKER_GRACE;
    let private = PrivateExec {
        secret_env: args.secret_env.clone(),
        error_token: to_hex(&rand::random::<[u8; 32]>()),
    };
    let started_exec = workers.start_exec(args, tmpdir, &private.error_token, kill_after);
    let mut child = match started_exec {
        Ok(child) => child,
        Err(error) => return Done::worker(error, audit),
    };
    let stdout = capture(child.stdout.take().expect("stdout is piped"));
    let stderr = capture(child.stderr.take().expect("stderr is piped"));
    // Fed from its own task while the output is read and the clock runs: a
    // program may read slowly, or never (then the write fails, which is
    // fine). Dropping the pipe at the end is the program's EOF.
    if let Some(mut pipe) = child.stdin.take() {
        let header = serde_json::to_vec(&private).expect("private exec request serializes");
        tokio::spawn(async move {
            if crate::wire::write_frame(&mut pipe, &header).await.is_ok()
                && let Some(bytes) = input
            {
                let _ = pipe.write_all(&bytes).await;
            }
        });
    }
    let (status, end) = wait_or_kill(&mut child, limits).await;
    // A Windows launcher killed here could not take its grants back; it has
    // ended, so its ledger is swept now rather than at the next start.
    #[cfg(windows)]
    if !matches!(end, End::Exited) {
        let state_dir = workers.state_dir.clone();
        let _ = tokio::task::spawn_blocking(move || crate::sandbox::sweep(&state_dir)).await;
    }
    let timed_out = matches!(end, End::TimedOut);
    // Both streams get the same grace, side by side.
    let ((stdout, stdout_cut), (stderr, stderr_cut)) = tokio::join!(
        collected(stdout, DRAIN_GRACE),
        collected(stderr, DRAIN_GRACE)
    );
    let duration_ms = started.elapsed().as_millis() as u64;

    // The worker failed before the program started: no execution result.
    if status.code() == Some(125)
        && let Some(error) =
            authenticated_worker_error(&String::from_utf8_lossy(&stderr), &private.error_token)
    {
        return Done::worker(error, audit);
    }

    let exit_code = status.code();
    #[cfg(unix)]
    let signal = status.signal().map(signal_name);
    #[cfg(windows)]
    let signal: Option<String> = None;
    audit["exit_code"] = json!(exit_code);
    audit["signal"] = json!(signal);
    audit["timed_out"] = json!(timed_out);
    audit["duration_ms"] = json!(duration_ms);
    // How much it wrote to stderr (never what), so the host's view can mark it.
    audit["stderr_bytes"] = json!(stderr.len());
    if let End::Cut(outcome) = end {
        let message = match outcome == code::interrupted {
            true => "the host stopped while the program ran, and killed it",
            false => "the caller went away while the program ran, so the host killed it",
        };
        return Done::failed(outcome, format!("{message} after {duration_ms} ms"), audit);
    }
    let mut result = json!({
        "writes": args.writes,
        "network": args.network,
        "exit_code": exit_code,
        "signal": signal,
        "timed_out": timed_out,
        "stdout": String::from_utf8_lossy(&stdout),
        "stderr": String::from_utf8_lossy(&stderr),
        "truncated": stdout_cut || stderr_cut,
    });
    // Windows: a listing that failed may be for want of the volume root's
    // entry (`latch setup`), so the agent is told how it is got.
    if (exit_code != Some(0) || !stderr.is_empty())
        && crate::sandbox::list_folders().is_some_and(|granted| granted != "granted")
    {
        result["hint"] = json!(LIST_FOLDERS_HINT);
    }
    Done {
        response: Response::Ok(result),
        audit_args: audit,
        outcome: if timed_out { "timed_out" } else { "ok" }.to_string(),
    }
}

/// What a failed exec says where agents may not list folders (Windows).
const LIST_FOLDERS_HINT: &str = "If it failed listing a folder: this Windows PC doesn't let \
agents list folders yet, so cmd's dir and vol are denied even in a folder you may write. Its \
owner allows it by running `latch setup`, which asks Windows for an administrator's \
permission once. Meanwhile `cmd /c for %f in (*) do @echo %f` lists one, and so does PowerShell's \
Get-ChildItem given the folder's full path (-LiteralPath).";

fn argv_is_usable(argv: &[String]) -> bool {
    !argv.is_empty() && !argv[0].is_empty()
}

/// Waits for the program, within `limits`. Past them, kills the program's
/// whole process group and returns its (killed) status, and why.
async fn wait_or_kill(
    child: &mut Child,
    limits: Limits<impl Future<Output = Code>>,
) -> (ExitStatus, End) {
    let end = tokio::select! {
        biased;
        () = tokio::time::sleep(Duration::from_secs(limits.timeout)) => End::TimedOut,
        status = child.wait() => match status {
            Ok(status) => return (status, End::Exited),
            Err(_) => End::TimedOut,
        },
        outcome = limits.cut_off => End::Cut(outcome),
    };
    // On Windows the child is the worker's launcher: its job dies with it,
    // and takes the program and all it started.
    #[cfg(windows)]
    let _ = child.start_kill();
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        // The worker started the program as the leader of a new process
        // group whose id is its pid, so this reaches every process in it.
        // SAFETY: std's child kill cannot reach descendants. This live child's
        // PID names the group we created for it; killpg takes only scalar arguments.
        unsafe { libc::killpg(pid as libc::pid_t, libc::SIGKILL) };
    }
    #[cfg(unix)]
    let killed = ExitStatus::from_raw(libc::SIGKILL);
    #[cfg(windows)]
    let killed = ExitStatus::from_raw(1);
    let status = child.wait().await.unwrap_or(killed);
    (status, end)
}

/// What has been read from one output stream so far. A reader task fills
/// it while `run` may already be looking (see `collected`), so it is shared
/// through an `Arc<Mutex<…>>`: `Arc` lets both hold it, `Mutex` lets one at
/// a time change it.
#[derive(Default)]
struct Captured {
    /// The first `OUTPUT_CAP` bytes.
    kept: Vec<u8>,
    /// Whether anything beyond them was dropped.
    cut: bool,
}

/// A stream being read in the background: its output so far, and the task.
struct Capture {
    captured: Arc<Mutex<Captured>>,
    task: JoinHandle<()>,
}

/// Starts reading `stream` in the background.
fn capture(stream: impl AsyncRead + Unpin + Send + 'static) -> Capture {
    let captured = Arc::new(Mutex::new(Captured::default()));
    let task = tokio::spawn(read_capped(stream, captured.clone()));
    Capture { captured, task }
}

/// Reads a stream to its end into `captured`, keeping the first
/// `OUTPUT_CAP` bytes and noting whether more came.
async fn read_capped(mut stream: impl AsyncRead + Unpin, captured: Arc<Mutex<Captured>>) {
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = match stream.read(&mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(read) => read,
        };
        let mut captured = captured.lock().expect("capture lock");
        let room = OUTPUT_CAP - captured.kept.len();
        captured.kept.extend_from_slice(&buffer[..read.min(room)]);
        captured.cut |= read > room;
    }
}

/// The output of a stream, once its reader reached the end, or after
/// `grace`, whichever comes first. A stream still open after the grace (a
/// background process the program left behind holds it) keeps everything
/// read so far, and is marked cut, since more may follow.
async fn collected(capture: Capture, grace: Duration) -> (Vec<u8>, bool) {
    let abort = capture.task.abort_handle();
    let finished = tokio::time::timeout(grace, capture.task).await.is_ok();
    if !finished {
        abort.abort();
    }
    let mut captured = capture.captured.lock().expect("capture lock");
    let kept = std::mem::take(&mut captured.kept);
    (kept, captured.cut || !finished)
}

/// A signal's usual name, e.g. 11 -> "SIGSEGV".
#[cfg(unix)]
fn signal_name(signal: i32) -> String {
    let name = match signal {
        libc::SIGHUP => "SIGHUP",
        libc::SIGINT => "SIGINT",
        libc::SIGQUIT => "SIGQUIT",
        libc::SIGILL => "SIGILL",
        libc::SIGTRAP => "SIGTRAP",
        libc::SIGABRT => "SIGABRT",
        libc::SIGBUS => "SIGBUS",
        libc::SIGFPE => "SIGFPE",
        libc::SIGKILL => "SIGKILL",
        libc::SIGUSR1 => "SIGUSR1",
        libc::SIGSEGV => "SIGSEGV",
        libc::SIGUSR2 => "SIGUSR2",
        libc::SIGPIPE => "SIGPIPE",
        libc::SIGALRM => "SIGALRM",
        libc::SIGTERM => "SIGTERM",
        libc::SIGSYS => "SIGSYS",
        other => return format!("SIG{other}"),
    };
    name.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn a_resolved_path_is_shown_plain_unless_that_names_something_else() {
        assert_eq!(
            shown(r"\\?\C:\Users\me\notes.txt"),
            r"C:\Users\me\notes.txt"
        );
        assert_eq!(
            shown(r"\\?\UNC\server\share\a.txt"),
            r"\\server\share\a.txt"
        );
        assert_eq!(shown(r"C:\already\plain"), r"C:\already\plain");
        // Plain, these would name another file, or a device.
        for kept in [
            r"\\?\C:\Users\me\notes.",
            r"\\?\C:\Users\me\notes ",
            r"\\?\C:\temp\NUL",
            r"\\?\C:\temp\con.txt",
            r"\\?\Volume{4f2a}\x",
        ] {
            assert_eq!(shown(kept), kept);
        }
    }
    use crate::failure::Exit;
    use std::os::unix::fs::PermissionsExt;
    use tokio::io::AsyncWriteExt;

    async fn run(workers: &WorkerSetup, args: &Value, cut_off: impl Future<Output = Code>) -> Done {
        match normalize(workers, args) {
            Ok(prepared) => super::run(workers, prepared, cut_off).await,
            Err(failure) => Done {
                response: Response::error(&failure),
                audit_args: Value::Null,
                outcome: failure.code,
            },
        }
    }

    #[tokio::test]
    async fn an_elapsed_timeout_wins_over_a_ready_exit() {
        let mut children = Vec::new();
        let mut inputs = Vec::new();
        for _ in 0..32 {
            let mut child = tokio::process::Command::new("/bin/sh")
                .args(["-c", "read line"])
                .stdin(std::process::Stdio::piped())
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            inputs.push(child.stdin.take().unwrap());
            children.push(child);
        }
        let mut waits: Vec<_> = children
            .iter_mut()
            .map(|child| {
                Box::pin(wait_or_kill(
                    child,
                    Limits {
                        timeout: 1,
                        cut_off: std::future::pending(),
                    },
                ))
            })
            .collect();
        std::future::poll_fn(|cx| {
            for wait in &mut waits {
                assert!(wait.as_mut().poll(cx).is_pending());
            }
            std::task::Poll::Ready(())
        })
        .await;
        // Like a frozen host: both the exits and the timers become ready
        // before it is able to poll the waits again.
        drop(inputs);
        std::thread::sleep(Duration::from_millis(1100));
        tokio::time::sleep(Duration::from_millis(10)).await;
        for wait in waits {
            let (_, end) = wait.await;
            assert!(
                matches!(end, End::TimedOut),
                "reported completion after timeout"
            );
        }
    }

    fn worker_setup() -> (tempfile::TempDir, WorkerSetup) {
        let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.dev/tests");
        std::fs::create_dir_all(&parent).unwrap();
        let dir = tempfile::tempdir_in(parent).unwrap();
        let program = dir.path().join("worker");
        std::fs::write(&program, "#!/bin/sh\necho started > worker-started\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let workers = WorkerSetup {
            home: dir.path().canonicalize().unwrap(),
            areas: vec![dir.path().to_path_buf()],
            reads: vec![],
            hidden: vec![],
            state_dir: dir.path().join("state"),
            program,
        };
        (dir, workers)
    }

    #[tokio::test]
    async fn malformed_exec_args_never_start_a_worker() {
        let (dir, workers) = worker_setup();
        for (field, values) in [
            (
                "secret_env",
                vec![json!(null), json!([]), json!({"TOKEN": 7})],
            ),
            ("network", vec![json!(null), json!("false"), json!(1)]),
            (
                "writes",
                vec![json!(null), json!("project"), json!([false])],
            ),
            (
                "timeout",
                vec![
                    json!(null),
                    json!("1"),
                    json!(true),
                    json!(1.5),
                    json!(-1),
                    json!(0),
                    json!(601),
                ],
            ),
            (
                "workdir",
                vec![json!(1), json!(false), json!([]), json!({})],
            ),
            (
                "argv",
                vec![
                    json!(null),
                    json!("true"),
                    json!([1]),
                    json!([]),
                    json!([""]),
                ],
            ),
            (
                "stdin",
                vec![json!("text"), json!(true), json!([-1]), json!([256])],
            ),
        ] {
            for (case, value) in values.into_iter().enumerate() {
                let mut args = json!({"argv": ["unused"]});
                args[field] = value;
                let done = run(&workers, &args, std::future::pending()).await;
                assert!(
                    !dir.path().join("worker-started").exists(),
                    "{field} case {case} started a worker"
                );
                let Response::Error { exit, code, .. } = done.response else {
                    panic!("{field} case {case} was accepted");
                };
                assert_eq!((exit, code.as_str()), (Exit::Usage, "bad_request"));
                assert_eq!(done.audit_args, Value::Null);
            }
        }
    }

    #[tokio::test]
    async fn positional_exec_args_never_start_a_worker() {
        let (dir, workers) = worker_setup();
        let done = run(
            &workers,
            &json!([["unused"], 60, null, null]),
            std::future::pending(),
        )
        .await;
        assert!(!dir.path().join("worker-started").exists());
        assert!(matches!(
            done.response,
            Response::Error { code, .. } if code == "bad_request"
        ));
        assert_eq!(done.audit_args, Value::Null);
    }

    #[tokio::test]
    async fn omitted_and_null_optional_exec_args_still_run() {
        let (dir, workers) = worker_setup();
        for args in [
            json!({"argv": ["unused"]}),
            json!({"argv": ["unused"], "workdir": null, "stdin": null}),
        ] {
            let done = run(&workers, &args, std::future::pending()).await;
            assert!(matches!(done.response, Response::Ok(_)));
            assert!(dir.path().join("worker-started").exists());
            assert_eq!(done.audit_args["workdir"], json!(workers.home));
        }
    }

    #[tokio::test]
    async fn declared_projects_support_cargo_build_and_npm_install() {
        let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.dev/tests");
        std::fs::create_dir_all(&parent).unwrap();
        let home = std::env::home_dir().unwrap().canonicalize().unwrap();
        // A gate container can mount the repository outside its HOME (/tmp).
        let parent = parent.canonicalize().unwrap();
        let dir = tempfile::tempdir_in(if parent.starts_with(&home) {
            &parent
        } else {
            &home
        })
        .unwrap();
        let root = dir.path().canonicalize().unwrap();
        let state = root.join("state");
        std::fs::create_dir(&state).unwrap();
        let binary = std::env::current_exe().unwrap();
        let workers = WorkerSetup {
            home: home.clone(),
            areas: vec![],
            reads: vec![],
            hidden: crate::sandbox::never_readable(&home),
            state_dir: state,
            program: binary.parent().unwrap().parent().unwrap().join("latch"),
        };
        let rust = root.join("rust-project");
        std::fs::create_dir_all(rust.join("src")).unwrap();
        std::fs::write(
            rust.join("Cargo.toml"),
            "[package]\nname='latch_exec_probe'\nversion='0.1.0'\nedition='2024'\n",
        )
        .unwrap();
        std::fs::write(rust.join("src/lib.rs"), "pub fn answer() -> u32 { 42 }\n").unwrap();
        let node = root.join("node-project");
        std::fs::create_dir(&node).unwrap();
        std::fs::write(
            node.join("package.json"),
            r#"{"name":"latch-exec-probe","version":"1.0.0","private":true}"#,
        )
        .unwrap();
        let rustup = std::env::var_os("RUSTUP_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".rustup"));
        for (project, argv) in [
            (
                &rust,
                vec![
                    "/usr/bin/env".to_string(),
                    format!("RUSTUP_HOME={}", rustup.display()),
                    format!("CARGO_HOME={}", rust.join("cache").display()),
                    "cargo".into(),
                    "build".into(),
                    "--offline".into(),
                ],
            ),
            (
                &node,
                vec![
                    "/usr/bin/env".into(),
                    format!("npm_config_cache={}", node.join("cache").display()),
                    "npm".into(),
                    "install".into(),
                    "--ignore-scripts".into(),
                    "--no-audit".into(),
                    "--no-fund".into(),
                ],
            ),
        ] {
            let args = json!({"argv": argv, "writes": [project], "network": true, "timeout": 60});
            let done = run(&workers, &args, std::future::pending()).await;
            let Response::Ok(result) = done.response else {
                panic!("{:?}", done.response)
            };
            assert_eq!(result["exit_code"], 0, "{result}");
        }
        assert!(rust.join("target/debug/liblatch_exec_probe.rlib").is_file());
        assert!(node.join("package-lock.json").is_file());
    }

    #[tokio::test]
    async fn secret_values_are_not_in_the_workers_process_arguments() {
        let (dir, mut workers) = worker_setup();
        let binary = std::env::current_exe().unwrap();
        workers.program = binary.parent().unwrap().parent().unwrap().join("latch");
        std::fs::create_dir(&workers.state_dir).unwrap();
        let tmp = dir.path().join("tmp");
        std::fs::create_dir(&tmp).unwrap();
        let marker = "worker-argv-private-marker-4a7d";
        let value = normalize(
            &workers,
            &json!({"argv": ["true"], "secret_env": {"TOKEN": marker}}),
        )
        .unwrap();
        let args = value.0;
        let mut child = workers
            .start_exec(&args, &tmp, "startup-test-token", 5)
            .unwrap();
        let pid = child.id().unwrap();
        let read_arguments = || {
            #[cfg(target_os = "linux")]
            let arguments = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap();
            #[cfg(target_os = "macos")]
            let arguments = {
                let output = std::process::Command::new("/bin/ps")
                    .args(["-p", &pid.to_string(), "-o", "command="])
                    .output()
                    .unwrap();
                assert!(output.status.success());
                output.stdout
            };
            String::from_utf8_lossy(&arguments).into_owned()
        };
        // spawn returns once the exec has closed the close-on-exec pipe, which
        // Linux does before it lays out the new argv: until then the process's
        // arguments read as empty.
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut arguments = read_arguments();
        while !arguments.contains("exec") && Instant::now() < deadline {
            assert!(!arguments.contains(marker));
            tokio::time::sleep(Duration::from_millis(10)).await;
            arguments = read_arguments();
        }
        assert!(arguments.contains("exec"), "{arguments:?}");
        assert!(!arguments.contains(marker));
        let header = serde_json::to_vec(&PrivateExec {
            secret_env: BTreeMap::new(),
            error_token: "startup-test-token".into(),
        })
        .unwrap();
        let mut input = child.stdin.take().unwrap();
        crate::wire::write_frame(&mut input, &header).await.unwrap();
        drop(input);
        assert!(child.wait().await.unwrap().success());
    }

    #[tokio::test]
    async fn output_is_kept_up_to_the_cap() {
        let grace = Duration::from_secs(1);
        let big = std::io::Cursor::new(vec![b'a'; OUTPUT_CAP + 10]);
        let (kept, cut) = collected(capture(big), grace).await;
        assert_eq!((kept.len(), cut), (OUTPUT_CAP, true));
        let small = std::io::Cursor::new(b"small".to_vec());
        let (kept, cut) = collected(capture(small), grace).await;
        assert_eq!((kept, cut), (b"small".to_vec(), false));
    }

    #[tokio::test]
    async fn output_read_before_the_grace_ran_out_is_kept() {
        // Like `sh -c 'printf hello; sleep 20 &'`: the program printed and
        // exited, but a background child still holds the pipe open.
        let (mut writer, reader) = tokio::io::duplex(1024);
        writer.write_all(b"hello").await.unwrap();
        let (kept, cut) = collected(capture(reader), Duration::from_millis(200)).await;
        assert_eq!(kept, b"hello");
        assert!(cut, "the stream was still open, so more may have followed");
        drop(writer);
    }

    #[test]
    fn signals_have_their_usual_names() {
        assert_eq!(signal_name(libc::SIGSEGV), "SIGSEGV");
        assert_eq!(signal_name(libc::SIGKILL), "SIGKILL");
    }
}

/// What the judge refuses on Windows alone.
#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    fn workers(home: &Path, program: PathBuf) -> WorkerSetup {
        WorkerSetup {
            home: home.to_path_buf(),
            areas: vec![],
            reads: vec![],
            hidden: crate::sandbox::never_readable(home),
            state_dir: home.join(".latch"),
            program,
        }
    }

    #[test]
    fn appdata_is_hidden_as_library_is_on_macos() {
        let home = std::env::home_dir().unwrap().canonicalize().unwrap();
        let workers = workers(&home, std::env::current_exe().unwrap());
        for (path, write) in [(r"~\AppData\Local", true), (r"~\AppData\Roaming", false)] {
            let path = resolve(&workers.home, Path::new(path)).unwrap();
            assert!(
                check_path(&workers, &path, write).is_err(),
                "{}",
                path.display()
            );
        }
    }

    #[test]
    fn no_write_path_holds_the_latch_the_host_runs() {
        let home = std::env::home_dir().unwrap().canonicalize().unwrap();
        let dir = tempfile::tempdir_in(&home).unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::create_dir_all(root.join("work")).unwrap();
        std::fs::write(root.join("bin").join("latch.exe"), "").unwrap();
        let workers = workers(&home, root.join("bin").join("latch.exe"));
        for held in [
            root.clone(),
            root.join("bin"),
            root.join("bin").join("version.dll"),
        ] {
            let failure = check_path(&workers, &held, true).unwrap_err();
            assert!(
                format!("{failure:?}").contains("the latch this host runs"),
                "{}: {failure:?}",
                held.display()
            );
        }
        check_path(&workers, &root.join("work"), true).unwrap();
        // Read, it is as any other folder.
        check_path(&workers, &root.join("bin"), false).unwrap();
    }

    /// The bot's probe on fe7a221: a worker its area lets delete
    /// `work/sub` swaps it for a junction into latch's state after another
    /// call's `work/sub/host.txt` was judged; the host's make of that missing
    /// folder must not land in the state.
    #[test]
    fn a_new_write_folder_is_made_in_the_judged_folder_not_through_a_link() {
        let home = std::env::home_dir().unwrap().canonicalize().unwrap();
        let dir = tempfile::tempdir_in(&home).unwrap();
        let root = dir.path().canonicalize().unwrap();
        let (sub, rules) = (
            root.join("work").join("sub"),
            root.join("state").join("rules"),
        );
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(&rules).unwrap();
        let judged = sub.join("host.txt");
        std::fs::rename(&sub, root.join("work").join("moved")).unwrap();
        let plain = |path: &Path| path.display().to_string().replace(r"\\?\", "");
        let linked = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J", &plain(&sub), &plain(&rules)])
            .output()
            .unwrap();
        assert!(linked.status.success(), "{linked:?}");
        assert!(create_write_dir(&judged).is_err());
        assert!(!rules.join("host.txt").exists(), "made in the state");
        // The folder that was judged, back in place, takes it.
        std::fs::remove_dir(&sub).unwrap();
        std::fs::rename(root.join("work").join("moved"), &sub).unwrap();
        create_write_dir(&judged).unwrap();
        assert!(judged.is_dir());
    }

    #[test]
    fn no_write_path_holds_the_state() {
        let home = std::env::home_dir().unwrap().canonicalize().unwrap();
        let dir = tempfile::tempdir_in(&home).unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("parent").join("state")).unwrap();
        let mut workers = workers(&home, std::env::current_exe().unwrap());
        workers.state_dir = root.join("parent").join("state");
        for held in [root.clone(), root.join("parent")] {
            let failure = check_path(&workers, &held, true).unwrap_err();
            assert!(
                format!("{failure:?}").contains("a write path may not hold one"),
                "{}: {failure:?}",
                held.display()
            );
        }
        check_path(&workers, &root, false).unwrap();
    }
}

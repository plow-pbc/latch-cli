//! The outer copy launches bubblewrap with a restricted filesystem view and
//! runs latch again inside it. Once launched, `confine` never returns in the
//! outer process. `LATCH_BWRAP_INNER` tells the inner copy to install seccomp
//! before returning to the worker's job.
//! The launch structure and socket filters follow OpenAI Codex (Apache-2.0).
//! See ../../NOTICE for attribution and the pinned source.

use std::collections::BTreeMap;
use std::io::{Read, Seek};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use seccompiler::{
    BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
    SeccompRule,
};

use super::{BrowserView, Policy, exit_with_status, reap};

type Error = Box<dyn std::error::Error>;
const INNER: &str = "LATCH_BWRAP_INNER";

fn bubblewrap() -> Result<PathBuf, String> {
    ["/usr/bin/bwrap", "/bin/bwrap"]
        .into_iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
        .ok_or_else(|| "bubblewrap (bwrap) is not installed; run install.sh to install the Linux sandbox prerequisites".to_string())
}

pub fn check() -> Result<(), String> {
    bubblewrap().map(|_| ())
}

/// Nothing to hold: the launcher outside bubblewrap watches the host.
pub struct HostWatch;

pub fn watch_host() -> Result<HostWatch, String> {
    Ok(HostWatch)
}

pub fn supervise(_watch: HostWatch, _kill_after: Option<u64>) -> Result<(), String> {
    Ok(())
}

pub fn confine(policy: &Policy) -> Result<(), String> {
    let result = if std::env::var_os(INNER).is_some() {
        filter(!policy.exec && policy.program.is_none(), false).and_then(|()| {
            if policy.exec {
                supervise_exec()?;
            }
            Ok(())
        })
    } else {
        launch(policy)
    };
    result.map_err(|err| format!("the bubblewrap sandbox could not be applied: {err}"))
}

fn launch(policy: &Policy) -> Result<(), Error> {
    if policy.break_for_test {
        return Err("forced sandbox-apply failure".into());
    }
    let program = std::env::current_exe()?.canonicalize()?;
    // A shared `/` is the whole view, writable; `exec` and file workers
    // read the whole filesystem, as Codex CLI's do (Google workers do not).
    // Either binds `/` first, so the mounts below (a minimal /dev, a private
    // /tmp, the areas) go on top.
    let root_shared = policy.areas.iter().any(|area| area == Path::new("/"));
    let reads_everything = policy.program.is_none();
    let root_bound = root_shared || reads_everything;
    let mut command = Command::new(bubblewrap()?);
    if root_shared {
        command.args(["--bind", "/", "/"]);
    } else if reads_everything {
        command.args(["--ro-bind", "/", "/"]);
    }
    command.args([
        "--unshare-user",
        "--unshare-pid",
        "--unshare-ipc",
        "--cap-drop",
        "ALL",
        "--die-with-parent",
        "--new-session",
        "--dev",
        "/dev",
    ]);
    mounts(&mut command, policy, &program)?;
    let status = if policy.exec {
        let folder = policy
            .tmpdir
            .as_ref()
            .ok_or("exec worker needs a temp folder")?;
        let file = tempfile::NamedTempFile::new_in(folder)?;
        // The program's TMPDIR is the private /tmp; the host's folder is
        // not in its view.
        command.args(["--setenv", "TMPDIR", "/tmp"]);
        // With `/` bound, /run is the host's, where bwrap cannot create the
        // status file; a private /run gives it somewhere to go.
        command.args(["--tmpfs", "/run"]);
        command.arg("--bind").arg(file.path()).arg(STATUS);
        Some(file)
    } else {
        None
    };
    // Hide state last, including any area or runtime mount that contains it.
    // A Google worker gets its own plugin's folder back, read-only: the one
    // part of the state folder a worker may see.
    let state = policy.state_dir.canonicalize()?;
    command.arg("--tmpfs").arg(&state);
    if let Some(gog) = &policy.program {
        let gog = gog.canonicalize()?;
        if gog.starts_with(&state) {
            let folder = gog.parent().unwrap_or(&gog);
            command.arg("--ro-bind").arg(folder).arg(folder);
        }
    }
    command.arg("--remount-ro").arg(&state);
    // Otherwise `/` is only the empty frame the mounts sit in: read-only.
    if !root_bound {
        command.args(["--remount-ro", "/"]);
    }
    command
        .arg("--chdir")
        .arg(std::env::current_dir()?)
        .args(["--setenv", INNER, "1", "--"])
        .arg(program)
        .args(std::env::args_os().skip(1));
    match status {
        Some(file) => launch_exec(&mut command, file, policy.kill_after),
        None => Err(command.exec().into()),
    }
}

#[allow(dead_code)] // the browser worker confines itself with it
pub fn confine_browser(view: &BrowserView) -> Result<(), String> {
    let result = if std::env::var_os(INNER).is_some() {
        filter(false, true)
    } else {
        std::env::current_exe()
            .and_then(|program| program.canonicalize())
            .map_err(Error::from)
            .and_then(|program| browser_command(view, &program))
            .and_then(|mut command| Err(command.exec().into()))
    };
    result.map_err(|err| format!("the bubblewrap sandbox could not be applied: {err}"))
}

/// What the engine and its display read: libraries and programs (Xvfb and
/// the xkbcomp it runs), fonts, name resolution, certificates, the time
/// zone, and the CPU list Chrome sizes itself by.
#[allow(dead_code)] // the browser worker confines itself with it
const BROWSER_SYSTEM: &[&str] = &[
    "/usr",
    "/lib",
    "/lib64",
    "/bin",
    "/etc/ld.so.cache",
    "/etc/resolv.conf",
    "/etc/hosts",
    "/etc/nsswitch.conf",
    "/etc/gai.conf",
    "/etc/ssl",
    "/etc/pki",
    "/etc/localtime",
    "/etc/fonts",
    "/var/cache/fontconfig",
    "/sys/devices/system/cpu",
];

/// bubblewrap running `program` (this latch) again, as a browser worker
/// inside `view`. The network is shared, for the engine's outbound
/// connections. Its own `/proc` shows only the sandbox's processes, which
/// Chrome's sandbox needs. A private `/tmp` holds the display's socket,
/// so nothing outside reaches the display.
#[allow(dead_code)] // the browser worker confines itself with it
fn browser_command(view: &BrowserView, program: &Path) -> Result<Command, Error> {
    let mut command = Command::new(bubblewrap()?);
    for path in BROWSER_SYSTEM {
        if Path::new(path).exists() {
            mount(&mut command, Path::new(path), true)?;
        }
    }
    command.args([
        "--unshare-user",
        "--unshare-pid",
        "--unshare-ipc",
        "--cap-drop",
        "ALL",
        "--die-with-parent",
        "--new-session",
        "--dev",
        "/dev",
        "--proc",
        "/proc",
        "--tmpfs",
        "/tmp",
        "--perms",
        "1777",
        "--dir",
        "/tmp/.X11-unix",
    ]);
    // Of the state folder, only the profile and the engine's plugin.
    let state = view.state_dir.canonicalize()?;
    command.arg("--tmpfs").arg(&state);
    mount(&mut command, &view.profile, false)?;
    // Missing until install.sh has run: the worker then says so itself.
    if view.plugin.exists() {
        mount(&mut command, &view.plugin, true)?;
    }
    command.arg("--remount-ro").arg(&state);
    mount(&mut command, &view.scratch, false)?;
    mount(&mut command, program, true)?;
    command
        .args(["--remount-ro", "/"])
        .arg("--chdir")
        .arg(std::env::current_dir()?)
        .args(["--setenv", INNER, "1", "--"])
        .arg(program)
        .args(std::env::args_os().skip(1));
    Ok(command)
}

fn mounts(command: &mut Command, policy: &Policy, program: &Path) -> Result<(), Error> {
    let mut runtime = if policy.exec {
        // The whole filesystem is already there, read-only (see `launch`).
        command.args([
            "--ro-bind",
            "/proc",
            "/proc",
            "--perms",
            "1777",
            "--tmpfs",
            "/tmp",
        ]);
        vec![]
    } else if policy.program.is_none() {
        // A file worker sees the whole filesystem too, but no processes.
        command.args(["--tmpfs", "/proc"]);
        vec![]
    } else {
        // Google workers get only runtime files, no system tools.
        vec![
            "/usr/lib",
            "/usr/lib64",
            "/lib",
            "/lib64",
            "/etc/ld.so.cache",
        ]
    };
    // Exec reuses Docker's masked procfs read-only (container PID metadata).
    // File and Google workers do not need procfs and get none (a file
    // worker's whole-filesystem view has an empty /proc).
    if !policy.exec && policy.program.is_some() {
        runtime.extend([
            "/etc/resolv.conf",
            "/etc/hosts",
            "/etc/nsswitch.conf",
            "/etc/gai.conf",
            "/etc/ssl",
            "/etc/pki",
            "/etc/localtime",
        ]);
    }
    if !policy.exec && policy.program.is_none() {
        command.arg("--unshare-net");
    }
    for path in runtime {
        if Path::new(path).exists() {
            mount(command, Path::new(path), true)?;
        }
    }
    for area in policy.areas.iter().filter(|area| *area != Path::new("/")) {
        mount(command, area, false)?;
    }
    // A worker could otherwise rename a folder above the state folder or the
    // running latch, inside an area, and put its own in their place for the
    // host's next start. Binding each such folder onto itself makes it a
    // mount point, which cannot be renamed or removed; it stays writable.
    for dir in folders_above(policy, program)? {
        mount(command, &dir, false)?;
    }
    // After the areas, so the running latch stays read-only even inside one.
    mount(command, program, true)?;
    if let Some(gog) = &policy.program {
        mount(command, gog, true)?;
    }
    Ok(())
}

/// The folders inside an area that hold the state folder or the running
/// latch, shallowest first. The areas themselves are already mount points.
fn folders_above(policy: &Policy, program: &Path) -> Result<Vec<PathBuf>, Error> {
    let areas: Vec<PathBuf> = policy
        .areas
        .iter()
        .map(|area| area.canonicalize())
        .collect::<Result<_, _>>()?;
    let mut dirs = Vec::new();
    for protected in [policy.state_dir.canonicalize()?, program.to_path_buf()] {
        for dir in protected.ancestors().skip(1) {
            if areas
                .iter()
                .any(|area| dir.starts_with(area) && dir != area)
            {
                dirs.push(dir.to_path_buf());
            }
        }
    }
    // Shallowest first, so each bind lands inside the one above it.
    dirs.sort();
    dirs.dedup();
    Ok(dirs)
}

fn launch_exec(
    command: &mut Command,
    mut file: tempfile::NamedTempFile,
    kill_after: Option<u64>,
) -> Result<(), Error> {
    // This copy's own time limit, past which SIGALRM ends it. bubblewrap
    // (`--die-with-parent`) then kills the sandbox and everything in it.
    // The host kills this copy's process group at the call's timeout,
    // before this. When the host itself dies, the kernel kills this copy
    // (the host sets that up as it starts it, see `worker.rs`), with the
    // same effect.
    if let Some(seconds) = kill_after {
        // SAFETY: std has no process alarm API. SIGALRM and SIG_DFL are valid
        // signal arguments, and alarm takes a bounded scalar number of seconds;
        // neither call accesses Rust memory or installs a Rust signal handler.
        unsafe {
            libc::signal(libc::SIGALRM, libc::SIG_DFL);
            libc::alarm(u32::try_from(seconds).unwrap_or(u32::MAX));
        }
    }
    // Bubblewrap reports both SIGSEGV and a normal exit 139 as 139. The inner
    // supervisor writes the actual wait status here, so this outer copy can
    // reproduce the child's exit code or signal without guessing. The host
    // removes the containing TMPDIR even when a timeout kills this copy.
    let exit = command.status()?;
    file.rewind()?;
    let mut raw = [0; 4];
    let reported = file.read_exact(&mut raw).is_ok();
    file.close()?;
    if reported {
        exit_with_status(std::process::ExitStatus::from_raw(i32::from_ne_bytes(raw)));
    }
    // Setup errors happen before the inner supervisor can report a status.
    std::process::exit(exit.code().unwrap_or(125));
}

const STATUS: &str = "/run/latch-exit-status";

fn supervise_exec() -> Result<(), Error> {
    // SAFETY: fork preserves the workload's raw wait status outside it. This
    // inner worker has not started other threads, so no other thread can leave
    // a Rust lock held in the child. Only the child returns to run the workload;
    // the parent waits and writes its status.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    if pid == 0 {
        return Ok(());
    }
    let status = reap(pid)?;
    std::fs::write(STATUS, status.into_raw().to_ne_bytes())?;
    std::process::exit(0);
}

fn mount(command: &mut Command, path: &Path, readonly: bool) -> Result<(), Error> {
    let real = path.canonicalize()?;
    // Preserve loader aliases such as /lib -> /usr/lib, but bind the resolved
    // source so a resolver symlink can never expose an unmounted host directory.
    command
        .arg(if readonly { "--ro-bind" } else { "--bind" })
        .arg(&real)
        .arg(if path.is_absolute() { path } else { &real });
    Ok(())
}

/// The seccomp filter; `offline` also refuses every socket (file workers).
/// `namespaces` lets the browser's engine make the namespaces its own
/// sandbox runs in; no other worker may.
fn filter(offline: bool, namespaces: bool) -> Result<(), Error> {
    let mut rules = BTreeMap::new();
    if !namespaces {
        rules.insert(libc::SYS_unshare, vec![]);
    }
    // No new mounts or access to processes outside this PID view.
    for syscall in [
        libc::SYS_mount,
        libc::SYS_umount2,
        libc::SYS_setns,
        libc::SYS_ptrace,
        libc::SYS_process_vm_readv,
        libc::SYS_process_vm_writev,
        libc::SYS_io_uring_setup,
        libc::SYS_io_uring_enter,
        libc::SYS_io_uring_register,
    ] {
        rules.insert(syscall, vec![]);
    }
    if offline {
        for syscall in [
            libc::SYS_socket,
            libc::SYS_socketpair,
            libc::SYS_connect,
            libc::SYS_sendto,
            libc::SYS_sendmsg,
            libc::SYS_sendmmsg,
        ] {
            rules.insert(syscall, vec![]);
        }
    } else {
        // VM sockets can reach host services outside the filesystem view.
        let deny_vsock = SeccompRule::new(vec![SeccompCondition::new(
            0,
            SeccompCmpArgLen::Dword,
            SeccompCmpOp::Eq,
            libc::AF_VSOCK as u64,
        )?])?;
        rules.insert(libc::SYS_socket, vec![deny_vsock.clone()]);
        rules.insert(libc::SYS_socketpair, vec![deny_vsock]);
    }
    let filter: BpfProgram = SeccompFilter::new(
        rules,
        SeccompAction::Allow,
        SeccompAction::Errno(libc::EPERM as u32),
        std::env::consts::ARCH.try_into()?,
    )?
    .try_into()?;
    // SAFETY: std cannot set no_new_privs before seccomp. This documented prctl
    // operation takes only integer arguments, touches no Rust memory, and
    // restricts the current worker; its threads and executed programs inherit it.
    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    seccompiler::apply_filter(&filter)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        net::{TcpListener, TcpStream},
    };

    #[test]
    fn a_browser_worker_sees_only_its_profile_plugin_and_scratch_of_the_host() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let state = root.join("state");
        let profile = state.join("browser/profiles/p");
        let plugin = state.join("plugins/cft/1");
        let scratch = root.join("scratch");
        for folder in [&profile, &plugin, &scratch] {
            fs::create_dir_all(folder).unwrap();
        }
        let view = BrowserView {
            state_dir: state.clone(),
            profile: profile.clone(),
            plugin: plugin.clone(),
            scratch: scratch.clone(),
        };
        let program = std::env::current_exe().unwrap().canonicalize().unwrap();
        let command = browser_command(&view, &program).unwrap();
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let has = |want: &[&str]| args.windows(want.len()).any(|w| w == want);
        let s = |path: &Path| path.to_str().unwrap().to_string();
        let at = |want: &[&str]| args.windows(want.len()).position(|w| w == want).unwrap();
        // The state folder is an empty tmpfs, with only these two bound in.
        assert!(has(&["--tmpfs", &s(&state)]));
        assert!(has(&["--bind", &s(&profile), &s(&profile)]));
        assert!(has(&["--ro-bind", &s(&plugin), &s(&plugin)]));
        assert!(has(&["--remount-ro", &s(&state)]));
        assert!(at(&["--tmpfs", &s(&state)]) < at(&["--bind", &s(&profile), &s(&profile)]));
        assert!(has(&["--bind", &s(&scratch), &s(&scratch)]));
        assert!(has(&["--ro-bind", &s(&program), &s(&program)]));
        // Nothing else of the host's files is writable, or there at all:
        // no area, no home, not the host's `/` or `/tmp`.
        let binds: Vec<&String> = args
            .windows(2)
            .filter(|w| w[0] == "--bind")
            .map(|w| &w[1])
            .collect();
        assert_eq!(binds, [&s(&profile), &s(&scratch)]);
        assert!(!has(&["--ro-bind", "/", "/"]));
        assert!(has(&["--tmpfs", "/tmp"]) && has(&["--remount-ro", "/"]));
        // Its own processes only; the network is shared, for the web.
        assert!(has(&["--proc", "/proc"]) && has(&["--unshare-pid"]));
        assert!(!args.iter().any(|arg| arg == "--unshare-net"));
    }

    #[test]
    fn threads_inherit_the_view_and_network_policy() {
        if let Ok(root) = std::env::var("LATCH_VIEW_TEST") {
            let root = PathBuf::from(root);
            let mode = std::env::var("LATCH_VIEW_MODE").unwrap();
            confine(&Policy {
                areas: vec![root.join("area")],
                state_dir: root.join("state"),
                exec: mode == "exec",
                tmpdir: (mode == "exec").then(|| root.join("outside")),
                kill_after: None,
                program: (mode == "google")
                    .then(|| PathBuf::from(std::env::var_os("LATCH_VIEW_PROGRAM").unwrap())),
                break_for_test: false,
            })
            .unwrap();
            std::thread::spawn(move || {
                assert_eq!(fs::read(root.join("area/inside")).unwrap(), b"inside");
                assert!(fs::read(root.join("state/secret")).is_err());
                // File and exec workers read the whole filesystem; Google's do not.
                assert_eq!(
                    fs::read(root.join("outside/other")).is_ok(),
                    mode != "google"
                );
                let connection = TcpStream::connect(std::env::var("LATCH_VIEW_ADDR").unwrap());
                if mode == "file" {
                    assert_eq!(
                        connection.unwrap_err().kind(),
                        std::io::ErrorKind::PermissionDenied
                    );
                    assert!(!Path::new("/proc/self").exists());
                } else {
                    assert!(connection.is_ok(), "{connection:?}");
                }
            })
            .join()
            .unwrap();
            return;
        }
        let parent = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.dev/tests");
        fs::create_dir_all(&parent).unwrap();
        let dir = tempfile::tempdir_in(parent).unwrap();
        for name in ["area", "state", "outside"] {
            fs::create_dir(dir.path().join(name)).unwrap();
        }
        fs::write(dir.path().join("area/inside"), "inside").unwrap();
        fs::write(dir.path().join("state/secret"), "secret").unwrap();
        fs::write(dir.path().join("outside/other"), "other").unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        for mode in ["file", "exec", "google"] {
            let child = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "sandbox::platform::tests::threads_inherit_the_view_and_network_policy",
                    "--nocapture",
                ])
                .current_dir(dir.path().join("area"))
                .env("LATCH_VIEW_TEST", dir.path().canonicalize().unwrap())
                .env("LATCH_VIEW_MODE", mode)
                .env("LATCH_VIEW_PROGRAM", std::env::current_exe().unwrap())
                .env(
                    "LATCH_VIEW_ADDR",
                    listener.local_addr().unwrap().to_string(),
                )
                .output()
                .unwrap();
            assert!(child.status.success(), "{mode}: {child:?}");
        }
    }
}

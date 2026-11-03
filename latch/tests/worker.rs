//! Runs the real `latch worker` against a throwaway area and state
//! directory, to check what the sandbox lets it touch.

// Most of these still run only on macOS and Linux, each `cfg` on the test.

use std::fs;
use std::io::Write;
#[cfg(target_os = "linux")]
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Output, Stdio};

use serde_json::Value;

mod portable;

/// An area holding `inside.txt` and a symlink into the state directory, a
/// state directory holding `secret`, and a file outside both.
fn setup() -> tempfile::TempDir {
    let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.dev/tests");
    fs::create_dir_all(&parent).unwrap();
    let dir = tempfile::tempdir_in(parent).unwrap();
    // `tmp` stands in for the per-call temp folder a host gives each exec.
    for sub in ["area", "state", "outside", "tmp"] {
        fs::create_dir(dir.path().join(sub)).unwrap();
    }
    fs::write(dir.path().join("area/inside.txt"), "inside").unwrap();
    fs::write(dir.path().join("state/secret"), "secret").unwrap();
    fs::write(dir.path().join("outside/other.txt"), "other").unwrap();
    // A link into the state (a file symlink, which a Windows standard user
    // may not make; the Unix sandboxes' tests use it).
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        dir.path().join("state/secret"),
        dir.path().join("area/link"),
    )
    .unwrap();
    dir
}

/// `latch worker --area area --denied state [extra] ACTION… -- PATH`, run
/// from inside the area with an empty environment, as a host runs it.
fn worker(dir: &Path, extra: &[&str], action: &[&str], path: &str, stdin: &[u8]) -> Output {
    let mut child = latch_worker()
        .env("USERPROFILE", portable::real_path(dir))
        .arg("worker")
        .arg("--area")
        .arg(folder(dir, "area"))
        .arg("--denied")
        .arg(folder(dir, "state"))
        .args(flags(dir, extra))
        .args(action)
        .args(["--", &at(dir, path)])
        .current_dir(dir.join("area"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // A worker that refuses exits without reading its stdin; that is fine.
    let _ = child.stdin.take().unwrap().write_all(stdin);
    child.wait_with_output().unwrap()
}

/// A folder of the test's as a worker is told it: from the area, or on
/// Windows by its real path, which is what its grants hold it to.
fn folder(dir: &Path, name: &str) -> String {
    at(dir, &format!("../{name}"))
}

/// `path`, written from the area, as a worker is told it (`folder`). An
/// absolute one stays.
fn at(dir: &Path, path: &str) -> String {
    if cfg!(unix) || Path::new(path).is_absolute() {
        return path.to_string();
    }
    let real = portable::real_path(dir);
    let found = match path.strip_prefix("../") {
        _ if path == ".." => real,
        Some(rest) => real.join(rest),
        None => real.join("area").join(path),
    };
    found.display().to_string().replace('/', "\\")
}

/// A worker's flags with each path in them as it is told one (`at`).
fn flags(dir: &Path, extra: &[&str]) -> Vec<String> {
    let mut told: Vec<String> = Vec::new();
    for flag in extra {
        let path = ["--area", "--read", "--hidden", "--denied"]
            .contains(&told.last().map_or("", String::as_str));
        told.push(if path {
            at(dir, flag)
        } else {
            flag.to_string()
        });
    }
    told
}

/// The latch under test as a host starts a worker: with an empty environment,
/// which no Windows process can start in a container with. There `worker`
/// and `exec_script` make the test's folder home, the one place a Windows
/// worker is ever given reads.
fn latch_worker() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_latch"));
    if cfg!(unix) {
        command.env_clear();
    }
    portable::find_tools(&mut command);
    command
}

/// Every exec starts with a private frame, even when it has no secrets.
fn exec_worker(command: &mut Command) -> Output {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let header = br#"{"secret_env":{},"error_token":"test-startup-token"}"#;
    if let Some(mut input) = child.stdin.take() {
        // A deliberately refused sandbox can close its input before setup.
        let _ = input
            .write_all(&(header.len() as u32).to_be_bytes())
            .and_then(|()| input.write_all(header));
    }
    child.wait_with_output().unwrap()
}

/// The code a worker reports for a path the sandbox keeps from it: Linux
/// leaves it out of the worker's view, macOS and Windows refuse it.
fn hidden_code() -> &'static str {
    if cfg!(target_os = "linux") {
        "not_found"
    } else {
        "sandbox_denied"
    }
}

/// The code on the worker's last stderr line.
fn code(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let last: Value = serde_json::from_str(stderr.lines().last().unwrap()).unwrap();
    assert_eq!(
        output.status.code(),
        Some(125),
        "a worker that fails exits 125"
    );
    last["worker_error"]["code"].as_str().unwrap().to_string()
}

#[test]
fn an_exec_requires_a_complete_private_header() {
    let dir = setup();
    for input in [b"".as_slice(), b"\0\0", b"\0\0\0\x01x"] {
        let output = worker(
            dir.path(),
            &["--tmpdir", &folder(dir.path(), "tmp")],
            &["exec"],
            "/bin/echo",
            input,
        );
        assert!(!output.status.success(), "{output:?}");
        assert_eq!(code(&output), "bad_request", "{output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
    }
}

#[test]
fn the_sandbox_reads_the_area_and_opt_ins_and_writes_only_the_area() {
    let dir = setup();
    let read = worker(dir.path(), &[], &["read"], "inside.txt", b"");
    assert!(read.status.success());
    assert_eq!(read.stdout, b"{\"size\":6}\ninside");

    let write = worker(
        dir.path(),
        &[],
        &["write", "--size", "7"],
        "new.txt",
        b"written",
    );
    assert!(write.status.success());
    assert_eq!(
        fs::read(dir.path().join("area/new.txt")).unwrap(),
        b"written"
    );

    // Outside the area, nothing is read, unless the owner opted in to it.
    let outside = worker(dir.path(), &[], &["read"], "../outside/other.txt", b"");
    assert_eq!(code(&outside), hidden_code());
    assert!(outside.stdout.is_empty());
    let opted_in = worker(
        dir.path(),
        &["--read", "../outside"],
        &["read"],
        "../outside/other.txt",
        b"",
    );
    assert!(opted_in.status.success(), "{opted_in:?}");
    assert_eq!(opted_in.stdout, b"{\"size\":5}\nother");

    // An unresolved link is refused; direct state access stays hidden.
    // (Only Unix has the link: `setup`.)
    for (path, expected) in [("link", "io_error"), ("../state/secret", hidden_code())] {
        if cfg!(windows) && path == "link" {
            continue;
        }
        let refused = worker(dir.path(), &[], &["read"], path, b"");
        assert_eq!(code(&refused), expected, "reading {path}");
        assert!(refused.stdout.is_empty(), "{refused:?}");
    }
    // Writes outside the area are refused, opted in or not (Linux: a
    // read-only view).
    let refused = worker(
        dir.path(),
        &["--read", "../outside"],
        &["write", "--size", "1"],
        "../outside/new.txt",
        b"x",
    );
    assert_eq!(code(&refused), "sandbox_denied");
    assert!(!dir.path().join("outside/new.txt").exists());
}

#[test]
fn missing_files_and_hidden_paths_report_the_operating_system_error() {
    let dir = setup();
    #[cfg(unix)]
    {
        let root = dir.path().canonicalize().unwrap();
        std::os::unix::fs::symlink(root.join("outside/missing"), root.join("area/dangling"))
            .unwrap();
        std::os::unix::fs::symlink(root.join("area/missing"), root.join("area/inside-link"))
            .unwrap();
    }
    // A symlink still in the worker's resolved path is refused before its
    // target is looked up, whether that target is hidden or missing.
    for (path, expected) in [
        ("missing", "not_found"),
        ("inside-link", "io_error"),
        ("missing-dir/file", "not_found"),
        ("../outside/missing", "not_found"),
        ("dangling", "io_error"),
        ("../state/missing", "not_found"),
    ] {
        // A Windows standard user may not make a file symlink.
        if cfg!(windows) && path.contains("link") | path.contains("dangling") {
            continue;
        }
        let output = worker(dir.path(), &[], &["read"], path, b"");
        assert_eq!(code(&output), expected, "{path}");
        assert!(output.stdout.is_empty(), "{output:?}");
    }
}

#[test]
fn a_write_worker_stages_a_new_file_in_its_temp_folder() {
    let dir = setup();
    let tmp = portable::real_path(&dir.path().join("tmp"));
    let tmpdir = tmp.to_str().unwrap();
    let upload = tmp.join("upload");
    let staged = |size: &str, bytes: &[u8]| {
        worker(
            dir.path(),
            &["--tmpdir", tmpdir],
            &["write", "--size", size],
            upload.to_str().unwrap(),
            bytes,
        )
    };
    // The transfer breaks off: 3 of the 10 promised bytes arrive.
    let short = staged("10", b"new");
    assert_eq!(code(&short), "transfer_incomplete");
    fs::remove_file(&upload).unwrap();
    // All bytes arrive.
    let full = staged("12", b"new contents");
    assert!(full.status.success(), "{full:?}");
    assert_eq!(fs::read(&upload).unwrap(), b"new contents");
    // A name already there is never written over (nor a link at it followed).
    let again = staged("1", b"x");
    assert!(!again.status.success(), "{again:?}");
    assert_eq!(fs::read(&upload).unwrap(), b"new contents");
}

#[test]
fn an_exec_worker_runs_programs_inside_the_sandbox() {
    let dir = setup();
    let exec = |argv: &[&str]| {
        exec_worker(
            latch_worker()
                .args(["worker", "--area"])
                .arg(folder(dir.path(), "area"))
                .arg("--denied")
                .arg(folder(dir.path(), "state"))
                .arg("--tmpdir")
                .arg(folder(dir.path(), "tmp"))
                .args(["exec", "--"])
                .arg(portable::sh())
                .args(argv)
                .current_dir(dir.path().join("area")),
        )
    };
    let pipe = exec(&["-c", "echo hello | tr a-z A-Z"]);
    assert!(pipe.status.success(), "{pipe:?}");
    assert_eq!(pipe.stdout, b"HELLO\n");

    let state = exec(&["-c", "cat ../state/secret"]);
    assert_eq!(state.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&state.stderr).contains(if cfg!(target_os = "macos") {
            "Operation not permitted"
        } else if cfg!(windows) {
            "Permission denied"
        } else {
            "No such file or directory"
        }),
        "{state:?}"
    );
    assert!(state.stdout.is_empty());

    let outside = exec(&["-c", "echo x > ../outside/new.txt"]);
    assert!(!outside.status.success());
    assert!(!dir.path().join("outside/new.txt").exists());
}

#[cfg(target_os = "linux")]
#[test]
fn an_exec_worker_cannot_create_nested_user_namespaces() {
    let dir = setup();
    let script = format!(
        r#"python3 - <<'PY'
import ctypes, errno, os, signal, threading
libc = ctypes.CDLL(None, use_errno=True)
libc.syscall.restype = ctypes.c_long
newuser = 0x10000000
clone_args = (ctypes.c_uint64 * 11)()
clone_args[0] = newuser
clone_args[4] = signal.SIGCHLD
for name, number, args in [
    ('unshare', {unshare}, (newuser,)),
    ('clone', {clone}, (newuser | signal.SIGCHLD, 0, 0, 0, 0)),
    ('clone3', {clone3}, (ctypes.byref(clone_args), ctypes.sizeof(clone_args))),
]:
    result = libc.syscall(number, *args)
    error = ctypes.get_errno()
    if name != 'unshare' and result == 0:
        os._exit(0)
    if result > 0:
        os.waitpid(result, 0)
    denied = (errno.EPERM, errno.ENOSPC)
    if name == 'clone3':
        denied += (errno.ENOSYS,)
    assert result == -1 and error in denied, (name, result, error)
    print(name, 'blocked:', error)
thread = threading.Thread(target=lambda: None)
thread.start()
thread.join()
print('threads still run')
PY"#,
        unshare = libc::SYS_unshare,
        clone = libc::SYS_clone,
        clone3 = libc::SYS_clone3,
    );
    for extra in [&[][..], &["--network"][..]] {
        let output = exec_script(dir.path(), extra, &script);
        assert!(output.status.success(), "{extra:?}: {output:?}");
        println!("{extra:?}: {}", String::from_utf8_lossy(&output.stdout));
    }
}

#[cfg(target_os = "macos")]
#[test]
fn an_exec_cannot_kill_its_supervisor() {
    let dir = setup();
    let output = exec_worker(
        Command::new(env!("CARGO_BIN_EXE_latch"))
            .args([
                "worker",
                "--area",
                "../area",
                "--denied",
                "../state",
                "--tmpdir",
                "../tmp",
                "exec",
                "--",
                "/bin/sh",
                "-c",
                "if kill -KILL \"$PPID\" 2>/dev/null; then echo escaped; else echo protected; fi",
            ])
            .current_dir(dir.path().join("area"))
            .env_clear(),
    );
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"protected\n", "{output:?}");
}

#[test]
fn an_agent_can_neither_read_nor_change_the_owners_rules() {
    let dir = setup();
    let rules = dir.path().join("state/rules");
    fs::create_dir(&rules).unwrap();
    fs::write(rules.join("host.txt"), "Never send email.").unwrap();
    // As latch makes a state folder, which on Windows is what keeps a grant
    // on the folder around it out.
    portable::make_private(&dir.path().join("state"));
    // On Windows no write area may hold the state, so it never gets this far.
    let around = worker(dir.path(), &["--area", ".."], &["read"], "inside.txt", b"");
    if cfg!(windows) {
        assert_eq!(code(&around), "sandbox_failed", "{around:?}");
    }
    let area = if cfg!(windows) {
        folder(dir.path(), "area")
    } else {
        at(dir.path(), "..")
    };
    let exec = |script: &str| {
        exec_worker(
            latch_worker()
                .args(["worker", "--area"])
                .arg(&area)
                .arg("--denied")
                .arg(folder(dir.path(), "state"))
                .arg("--tmpdir")
                .arg(folder(dir.path(), "tmp"))
                .args(["exec", "--"])
                .arg(portable::sh())
                .args(["-c", script])
                .current_dir(dir.path().join("area")),
        )
    };
    for script in [
        "cat ../state/rules/host.txt",
        "echo 'Anything goes.' > ../state/rules/host.txt",
        "rm ../state/rules/host.txt",
        "echo 'Anything goes.' > ../state/rules/0123456789abcdef0123456789abcdef.txt",
        "mv ../state/rules ../state/gone",
    ] {
        let output = exec(script);
        assert!(!output.status.success(), "{script}: {output:?}");
        assert!(output.stdout.is_empty(), "{script}");
        // The program ran and was refused: not a worker that never started.
        assert!(
            !String::from_utf8_lossy(&output.stderr).contains("worker_error"),
            "{script}: {output:?}"
        );
    }
    let read = worker(
        dir.path(),
        &["--area", ".."],
        &["read"],
        "../state/rules/host.txt",
        b"",
    );
    assert!(
        ["not_found", "sandbox_denied", "sandbox_failed"].contains(&code(&read).as_str()),
        "{read:?}"
    );
    assert!(read.stdout.is_empty());
    let write = worker(
        dir.path(),
        &["--area", ".."],
        &["write", "--size", "1"],
        "../state/rules/host.txt",
        b"x",
    );
    assert!(!write.status.success(), "{write:?}");
    assert_eq!(
        fs::read_dir(&rules).unwrap().count(),
        1,
        "nothing added or removed"
    );
    assert_eq!(
        fs::read_to_string(rules.join("host.txt")).unwrap(),
        "Never send email."
    );
}

#[test]
fn a_workdir_outside_the_areas_is_refused() {
    let dir = setup();
    let real = portable::real_path(dir.path());
    let area = real.join("area");
    for workdir in [real.join("outside"), real.join("state")] {
        let output = exec_worker(
            latch_worker()
                .args(["worker", "--area"])
                .arg(&area)
                .arg("--denied")
                .arg(real.join("state"))
                .arg("--tmpdir")
                .arg(real.join("tmp"))
                .args(["exec", "--workdir"])
                .arg(&workdir)
                .arg("--")
                .arg(portable::sh())
                .args(["-c", "pwd; ls"])
                .current_dir(&area),
        );
        // On Windows an exec reads no home, so a folder the host judged may be
        // run in, entered with nothing in it listed; the state still may not.
        if cfg!(windows) && workdir.ends_with("outside") {
            assert_eq!(output.status.code(), Some(1), "{output:?}");
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("can't open '.'"),
                "{output:?}"
            );
            assert!(
                String::from_utf8_lossy(&output.stdout).ends_with("outside\n"),
                "{output:?}"
            );
            continue;
        }
        if cfg!(windows) {
            assert_eq!(code(&output), "sandbox_failed", "{output:?}");
            continue;
        }
        assert!(output.stdout.is_empty(), "{output:?}");
        // An exec worker sees the whole filesystem, so latch's own check
        // refuses a workdir outside the areas, on Linux as on macOS.
        assert_eq!(
            code(&output),
            "workdir_outside_areas",
            "{}",
            workdir.display()
        );
    }
}

#[test]
fn a_sandbox_that_cannot_be_applied_stops_the_worker() {
    let dir = setup();
    let output = worker(
        dir.path(),
        &["--break-sandbox-for-test"],
        &["read"],
        "inside.txt",
        b"",
    );
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(code(&output), "sandbox_failed");
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
#[test]
fn without_a_sandbox_for_this_system_no_worker_runs() {
    let dir = setup();
    let output = worker(dir.path(), &[], &["read"], "inside.txt", b"");
    assert!(output.stdout.is_empty());
    assert_eq!(code(&output), "sandbox_failed");
}

#[cfg(target_os = "linux")]
#[test]
fn an_area_can_contain_state_but_cannot_read_or_write_it() {
    let dir = setup();
    let inside = worker(dir.path(), &["--area", ".."], &["read"], "inside.txt", b"");
    assert!(inside.status.success(), "{inside:?}");
    assert_eq!(inside.stdout, b"{\"size\":6}\ninside");
    for (path, expected) in [("../state/secret", "not_found"), ("link", "io_error")] {
        let hidden = worker(dir.path(), &["--area", ".."], &["read"], path, b"");
        assert_eq!(code(&hidden), expected, "{path}");
        assert!(hidden.stdout.is_empty(), "{hidden:?}");
    }
    let write = worker(
        dir.path(),
        &["--area", ".."],
        &["write", "--size", "1"],
        "../state/new",
        b"x",
    );
    assert!(!write.status.success(), "{write:?}");
    assert!(!dir.path().join("state/new").exists());
    assert_eq!(
        fs::read(dir.path().join("state/secret")).unwrap(),
        b"secret"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_worker_confines_itself_before_starting_threads() {
    let dir = setup();
    let mut child = Command::new(env!("CARGO_BIN_EXE_latch"))
        .args([
            "worker", "--area", "../area", "--denied", "../state", "write", "--size", "1", "--",
            "new.txt",
        ])
        .current_dir(dir.path().join("area"))
        .env_clear()
        .env("TOKIO_WORKER_THREADS", "2")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut ready = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut ready)
        .unwrap();
    // The worker has applied its sandbox and is waiting for our bytes.
    let threads = fs::read_dir(format!("/proc/{}/task", child.id()))
        .unwrap()
        .count();
    child.stdin.take().unwrap().write_all(b"x").unwrap();
    let output = child.wait_with_output().unwrap();

    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        serde_json::from_str::<Value>(&ready).unwrap()["ready"],
        true
    );
    assert_eq!(threads, 1, "no unconfined runtime threads may exist");
    assert_eq!(fs::read(dir.path().join("area/new.txt")).unwrap(), b"x");
}

/// `latch serve` with no login, run with a seccomp filter
/// (installed in the child before it starts) that makes `syscall` fail with
/// EPERM when `rules` match. Without the filter it stops at the login check.
#[cfg(target_os = "linux")]
fn serve_with_denied(rules: Option<(i64, Vec<seccompiler::SeccompRule>)>) -> Output {
    use std::os::unix::process::CommandExt;

    let dir = setup();
    let state = tempfile::tempdir_in(dir.path()).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_latch"));
    command
        .args(["serve", "--foreground", "--state-dir"])
        .arg(state.path())
        .env_clear()
        .env("HOME", dir.path())
        .env("LATCH_TEST_LOOPBACK", "1");
    if let Some((syscall, conditions)) = rules {
        let filter: seccompiler::BpfProgram = seccompiler::SeccompFilter::new(
            [(syscall, conditions)].into_iter().collect(),
            seccompiler::SeccompAction::Allow,
            seccompiler::SeccompAction::Errno(libc::EPERM as u32),
            std::env::consts::ARCH.try_into().unwrap(),
        )
        .unwrap()
        .try_into()
        .unwrap();
        // SAFETY: between fork and exec the child only installs the filter
        // compiled above, which allocates nothing.
        unsafe {
            command.pre_exec(move || {
                seccompiler::apply_filter(&filter).map_err(|_| std::io::Error::last_os_error())
            });
        }
    }
    command.output().unwrap()
}

#[cfg(target_os = "linux")]
#[test]
fn serve_refuses_to_start_when_the_sandbox_cannot_be_installed() {
    use seccompiler::{SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompRule};

    // The helper exists, but setting up mounts is refused.
    let restrict = Some((libc::SYS_mount, vec![]));
    // seccomp exists and reports its actions, but a new filter is refused.
    let set_filter = SeccompCondition::new(
        0,
        SeccompCmpArgLen::Dword,
        SeccompCmpOp::Eq,
        libc::SECCOMP_SET_MODE_FILTER as u64,
    )
    .unwrap();
    let install = Some((
        libc::SYS_seccomp,
        vec![SeccompRule::new(vec![set_filter]).unwrap()],
    ));
    for rules in [restrict, install] {
        let output = serve_with_denied(rules);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "{stderr}");
        assert!(stderr.contains("sandbox_unavailable"), "{stderr}");
    }
    // Where the sandbox can be installed, startup goes on to the login check.
    let output = serve_with_denied(None);
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("not_logged_in"),
        "{output:?}"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn exec_preserves_signals_and_does_not_confuse_them_with_exit_codes() {
    use std::os::unix::process::ExitStatusExt;
    let dir = setup();
    for (script, code, signal) in [
        ("exit 139", Some(139), None),
        ("kill -SEGV $$", None, Some(libc::SIGSEGV)),
    ] {
        let output = exec_worker(
            Command::new(env!("CARGO_BIN_EXE_latch"))
                .args([
                    "worker", "--area", "../area", "--denied", "../state", "--tmpdir", "../tmp",
                    "exec", "--", "/bin/sh", "-c", script,
                ])
                .current_dir(dir.path().join("area"))
                .env_clear(),
        );
        assert_eq!(output.status.code(), code, "{output:?}");
        assert_eq!(output.status.signal(), signal, "{output:?}");
    }
}

#[test]
fn an_exec_worker_writes_only_its_own_temp_folder_outside_the_areas() {
    let dir = setup();
    let real = portable::real_path(dir.path());
    // Two per-call folders in the OS temp folder, as a host makes them.
    let mine = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    // The refused write is in a subshell: how a shell exits when a
    // redirection fails differs (bash 1, dash 2).
    let script =
        r#"echo x > "$TMPDIR/t" && cat "$TMPDIR/t"; (echo y > "$0/stolen") && echo stolen; exit 0"#;
    let output = exec_worker(
        latch_worker()
            .arg("worker")
            .arg("--area")
            .arg(real.join("area"))
            .arg("--denied")
            .arg(real.join("state"))
            .arg("--tmpdir")
            .arg(portable::real_path(mine.path()))
            .args(["exec", "--"])
            .arg(portable::sh())
            .args(["-c", script])
            .arg(portable::real_path(other.path()))
            .current_dir(real.join("area"))
            .env("TMPDIR", portable::real_path(mine.path())),
    );
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"x\n", "{output:?}");
    if !cfg!(target_os = "linux") {
        // macOS and Windows: the program's TMPDIR is the folder it was given,
        // and nothing else of the OS temp folder.
        assert!(mine.path().join("t").exists());
        assert!(!other.path().join("stolen").exists());
    } else {
        // Linux: the private /tmp; the host's folders are not in its view.
        assert!(!mine.path().join("t").exists());
        assert!(!other.path().join("stolen").exists());
    }
}

#[test]
fn exec_reads_and_writes_only_areas_never_state_or_latch() {
    // The latch running the worker and the state folder both sit inside the
    // area, as when a shared folder holds them.
    let dir = setup();
    let real = portable::real_path(dir.path());
    let area = real.join("area");
    fs::create_dir_all(area.join("bin")).unwrap();
    fs::create_dir_all(area.join("keep/.state")).unwrap();
    fs::write(area.join("keep/.state/secret"), "secret").unwrap();
    portable::make_private(&area.join("keep/.state"));
    let latch = area.join(format!("bin/latch{}", std::env::consts::EXE_SUFFIX));
    fs::copy(env!("CARGO_BIN_EXE_latch"), &latch).unwrap();
    let before = fs::read(&latch).unwrap();
    let script = r#"
        cat ../outside/other.txt && echo " READ-OUTSIDE"
        cat inside.txt && echo " read-inside-ok"
        echo x > ../outside/new.txt && echo "WROTE-OUTSIDE"
        cat keep/.state/secret; echo x > keep/.state/new && echo "WROTE-STATE"
        echo x > bin/latch && echo "WROTE-LATCH"
        rm -f bin/latch && echo "REMOVED-LATCH"
        ln bin/latch hard && echo x >> hard && echo "WROTE-HARDLINK"
        mv bin bin2 && echo "MOVED-LATCH"
        mkdir -p bin && echo planted > bin/latch && echo "PLANTED-LATCH"
        mv keep keep2 && echo "MOVED-STATE" && cat keep2/.state/secret
        mkdir -p keep/.state && echo planted > keep/.state/config.json && echo "PLANTED-STATE"
        cat */.state/secret
        true
    "#;
    let mut own = Command::new(&latch);
    if cfg!(unix) {
        own.env_clear();
    }
    portable::find_tools(&mut own);
    let output = exec_worker(
        own.arg("worker")
            .arg("--area")
            .arg(&area)
            .arg("--denied")
            .arg(area.join("keep/.state"))
            .arg("--tmpdir")
            .arg(real.join("tmp"))
            .args(["exec", "--"])
            .arg(portable::sh())
            .args(["-c", script])
            .current_dir(&area),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    // On Windows no write area may hold the state or the latch it runs.
    if cfg!(windows) {
        assert_eq!(code(&output), "sandbox_failed", "{output:?}");
    } else {
        assert!(output.status.success(), "{output:?}");
        assert!(stdout.contains("inside read-inside-ok"), "{stdout}");
    }
    for escape in [
        "READ-", "WROTE-", "REMOVED-", "MOVED-", "PLANTED-", "secret",
    ] {
        assert!(!stdout.contains(escape), "{escape} in {stdout}");
    }
    assert!(!real.join("outside/new.txt").exists());
    assert!(!area.join("keep/.state/new").exists());
    assert_eq!(
        fs::read(&latch).unwrap(),
        before,
        "latch unchanged, in place"
    );
    assert!(!area.join("bin2").exists() && !area.join("keep2").exists());
    assert!(!area.join("keep/.state/config.json").exists());
}

/// A get's worker, as a host starts it: home readable, less latch's state
/// and (macOS) ~/Library apart from its Caches, and nothing writable.
#[test]
fn a_read_worker_reads_home_but_not_what_is_hidden() {
    let dir = setup();
    let home = portable::real_path(&dir.path().join("area"));
    for (path, text) in [
        (".ssh/config", "Host example"),
        (".latch/key", "made-up key"),
        ("Library/Messages/chat.db", "made-up chat"),
        ("Library/Caches/tool/cache", "made-up cache"),
    ] {
        fs::create_dir_all(home.join(path).parent().unwrap()).unwrap();
        fs::write(home.join(path), text).unwrap();
    }
    if cfg!(windows) {
        portable::make_private(&home.join(".latch"));
    }
    let denied = folder(dir.path(), "state");
    let read = |path: &str| {
        let home = home.to_str().unwrap();
        let mut args = vec![
            "worker", "--home", home, "--read", home, "--denied", &denied,
        ];
        let separator = std::path::MAIN_SEPARATOR;
        let hidden = [
            format!("{home}{separator}.latch"),
            format!("{home}{separator}Library"),
        ];
        let hidden: Vec<&str> = hidden.iter().map(String::as_str).collect();
        let shown = if cfg!(target_os = "macos") {
            &hidden[..]
        } else {
            &hidden[..1]
        };
        for path in shown {
            args.extend(["--hidden", path]);
        }
        let path = format!("{home}/{path}");
        let path = if cfg!(windows) {
            path.replace('/', "\\")
        } else {
            path
        };
        latch_worker()
            .env("USERPROFILE", home)
            .args(&args)
            .args(["read", "--", &path])
            .current_dir(dir.path().join("area"))
            .output()
            .unwrap()
    };
    let ssh = read(".ssh/config");
    assert!(ssh.status.success(), "{ssh:?}");
    assert!(String::from_utf8_lossy(&ssh.stdout).ends_with("Host example"));
    assert_eq!(code(&read(".latch/key")), hidden_code());
    if cfg!(target_os = "macos") {
        assert_eq!(code(&read("Library/Messages/chat.db")), hidden_code());
        let cache = read("Library/Caches/tool/cache");
        assert!(cache.status.success(), "{cache:?}");
    }
    // Nothing outside home, either.
    assert!(!read("../outside/other.txt").status.success());
}

#[test]
fn a_read_refuses_a_symlink_swapped_in_after_resolution_before_sending_bytes() {
    for component in 0..3 {
        let dir = setup();
        let area = dir.path().join("area").canonicalize().unwrap();
        let path = area.join("tree/inner/note.txt");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "judged bytes").unwrap();
        let resolved = path.canonicalize().unwrap();
        let target = resolved.ancestors().nth(component).unwrap();
        let replacement = area.join("replacement");
        let file = match component {
            0 => replacement.clone(),
            1 => replacement.join("note.txt"),
            _ => replacement.join("inner/note.txt"),
        };
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, "unjudged bytes").unwrap();
        fs::rename(target, target.with_file_name("saved")).unwrap();
        // The file itself, by a file symlink, which a Windows standard user
        // may not make; a folder by a folder link anywhere.
        #[cfg(unix)]
        std::os::unix::fs::symlink(&replacement, target).unwrap();
        #[cfg(windows)]
        if component == 0 {
            continue;
        } else {
            portable::link_folder(&replacement, target);
        }

        let read = worker(dir.path(), &[], &["read"], resolved.to_str().unwrap(), b"");
        assert!(!read.status.success(), "component {component}: {read:?}");
        assert!(read.stdout.is_empty(), "component {component}: {read:?}");
    }
}

/// The `PATH` a host gives exec's programs (Unix; `latch_worker` on Windows).
const HOST_PATH: &str = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";

/// `latch worker … exec -- /bin/sh -c SCRIPT`, as a host runs it: from the
/// area, with HOME there too, and `extra` worker flags.
fn exec_script(dir: &Path, extra: &[&str], script: &str) -> Output {
    let real = portable::real_path(dir);
    let mut command = latch_worker();
    if cfg!(unix) {
        command.env("PATH", HOST_PATH);
    }
    exec_worker(
        command
            .args(["worker", "--area"])
            .arg(folder(dir, "area"))
            .arg("--denied")
            .arg(folder(dir, "state"))
            .args(flags(dir, extra))
            .arg("--tmpdir")
            .arg(folder(dir, "tmp"))
            .args(["exec", "--"])
            .arg(portable::sh())
            .args(["-c", script])
            .current_dir(real.join("area"))
            .env("USERPROFILE", &real)
            .env("HOME", real.join("area"))
            .env("TMPDIR", real.join("tmp")),
    )
}

#[test]
fn everyday_tools_work_reading_only_the_area_and_the_system() {
    let dir = setup();
    let installed = |tool: &str| {
        if cfg!(windows) {
            // What `sh` runs by itself, and what is on the runner's PATH.
            let path = std::env::var_os("PATH").unwrap_or_default();
            return tool == "ls"
                || std::env::split_paths(&path)
                    .any(|folder| folder.join(format!("{tool}.exe")).is_file());
        }
        HOST_PATH
            .split(':')
            .any(|folder| Path::new(folder).join(tool).is_file())
    };
    let battery = [
        // (Windows: `-a` looks at `..` too, which a container may not see.)
        (
            "ls",
            if cfg!(windows) {
                "ls -l && grep -rn inside . && pwd"
            } else {
                "ls -la && grep -rn inside . && pwd"
            },
        ),
        (
            "git",
            if cfg!(windows) {
                "git --version"
            } else {
                "git init -q repo && cd repo && echo hi > a && git add a \
                 && git -c user.name=t -c user.email=t@example.com commit -qm one \
                 && git status --short && git log --oneline && git diff HEAD"
            },
        ),
        (
            "python3",
            "python3 -c 'import ssl, json, sqlite3; print(ssl.OPENSSL_VERSION)'",
        ),
        ("node", "node -e 'console.log(require(\"os\").platform())'"),
    ];
    // Git opens the null device for itself, so on Windows it starts once an
    // administrator has opened it: CI's step for that runs this test again
    // with this set. Past `--version` it needs more than that, and node and
    // python are only as open to a container as their installs make them.
    let nul_closed = cfg!(windows) && std::env::var_os("LATCH_TEST_NULL_DEVICE_OPEN").is_none();
    for (tool, script) in battery {
        if cfg!(windows) && (tool == "python3" || tool == "node" || (nul_closed && tool == "git")) {
            continue;
        }
        // A tool this machine lacks cannot be tried here.
        if !installed(tool) {
            eprintln!("{tool} is not installed here: not tried");
            continue;
        }
        let output = exec_script(dir.path(), &[], script);
        assert!(output.status.success(), "{tool}: {output:?}");
    }
}

#[test]
fn exec_reads_outside_the_area_only_what_the_owner_opted_in_to() {
    let dir = setup();
    // `outside` stands for the owner's home folder, with a key in it.
    fs::create_dir(dir.path().join("outside/.ssh")).unwrap();
    fs::write(dir.path().join("outside/.ssh/id_ed25519"), "PRIVATE KEY").unwrap();
    let script = "cat ../outside/other.txt; cat ../outside/.ssh/id_ed25519; ls ../outside; true";
    let confined = exec_script(dir.path(), &[], script);
    let stdout = String::from_utf8_lossy(&confined.stdout);
    assert!(confined.status.success(), "{confined:?}");
    assert!(
        !stdout.contains("other") && !stdout.contains("PRIVATE"),
        "{stdout}"
    );
    let opted_in = exec_script(dir.path(), &["--read", "../outside"], script);
    let stdout = String::from_utf8_lossy(&opted_in.stdout);
    assert!(stdout.contains("other"), "{opted_in:?}");
    // Still read-only.
    let write = exec_script(
        dir.path(),
        &["--read", "../outside"],
        "echo x > ../outside/new.txt",
    );
    assert!(!write.status.success(), "{write:?}");
    assert!(!dir.path().join("outside/new.txt").exists());
}

#[test]
fn credentials_stay_hidden_inside_an_opt_in_and_inside_the_area() {
    let dir = setup();
    let real = portable::real_path(dir.path());
    for home in ["outside", "area"] {
        fs::create_dir(real.join(home).join(".ssh")).unwrap();
        fs::write(real.join(home).join(".ssh/id_ed25519"), "PRIVATE KEY").unwrap();
        fs::write(real.join(home).join(".netrc"), "password hunter2").unwrap();
        // As hidden paths are on Windows, or a grant around them fails: `sealed`.
        if cfg!(windows) {
            portable::make_private(&real.join(home).join(".ssh"));
            portable::make_private(&real.join(home).join(".netrc"));
        }
    }
    let flags = [
        "--read",
        "../outside",
        "--hidden",
        "../outside/.ssh",
        "--hidden",
        "../outside/.netrc",
        "--hidden",
        ".ssh",
        "--hidden",
        ".netrc",
    ];
    // On Windows no write area may hold a hidden path, so those stay out.
    let (flags, area_held) = if cfg!(windows) {
        let refused = worker(dir.path(), &flags, &["read"], "inside.txt", b"");
        assert_eq!(code(&refused), "sandbox_failed", "{refused:?}");
        (&flags[..6], false)
    } else {
        (&flags[..], true)
    };
    for path in [
        "../outside/.ssh/id_ed25519",
        "../outside/.netrc",
        ".ssh/id_ed25519",
        ".netrc",
    ] {
        if !area_held && !path.starts_with("../") {
            continue;
        }
        let read = worker(dir.path(), &flags, &["read"], path, b"");
        let stdout = String::from_utf8_lossy(&read.stdout);
        assert!(
            !stdout.contains("PRIVATE") && !stdout.contains("hunter2"),
            "{path}: {read:?}"
        );
    }
    let opted_in = worker(dir.path(), &flags, &["read"], "../outside/other.txt", b"");
    assert_eq!(opted_in.stdout, b"{\"size\":5}\nother");

    let script = if area_held {
        r#"
        cat ../outside/.ssh/id_ed25519 ../outside/.netrc .ssh/id_ed25519 .netrc
        echo key >> .ssh/authorized_keys && echo "WROTE-SSH"
        mv .ssh moved && echo "MOVED-SSH" && cat moved/id_ed25519
        cat inside.txt
        true
    "#
    } else {
        r#"
        cat ../outside/.ssh/id_ed25519 ../outside/.netrc
        echo key >> ../outside/.ssh/authorized_keys && echo "WROTE-SSH"
        mv ../outside/.ssh ../outside/moved && echo "MOVED-SSH"
        cat inside.txt
        true
    "#
    };
    let output = exec_script(dir.path(), flags, script);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(stdout, "inside", "{output:?}");
    assert!(!real.join("area/.ssh/authorized_keys").exists());
    assert_eq!(
        fs::read(real.join("area/.ssh/id_ed25519")).unwrap(),
        b"PRIVATE KEY"
    );
}

#[test]
fn a_hidden_path_can_be_neither_hard_linked_nor_moved_out_of_reach() {
    let dir = setup();
    let real = portable::real_path(dir.path());
    // A hidden folder and a hidden file, each below a folder of the area.
    fs::create_dir_all(real.join("area/nest/peer")).unwrap();
    fs::write(real.join("area/nest/peer/key"), "PRIVATE KEY").unwrap();
    fs::create_dir_all(real.join("area/conf")).unwrap();
    fs::write(real.join("area/conf/.netrc"), "password hunter2").unwrap();
    if cfg!(windows) {
        portable::make_private(&real.join("area/nest/peer"));
        portable::make_private(&real.join("area/conf/.netrc"));
    }
    let flags = ["--hidden", "nest/peer", "--hidden", "conf/.netrc"];
    let script = r#"
        ln nest/peer/key stolen-key && echo "LINKED-KEY"
        ln conf/.netrc stolen-netrc && echo "LINKED-NETRC"
        cat stolen-key stolen-netrc
        mv nest moved && echo "MOVED-NEST"
        mv conf moved-conf && echo "MOVED-CONF"
        true
    "#;
    let output = exec_script(dir.path(), &flags, script);
    let stdout = String::from_utf8_lossy(&output.stdout);
    // On Windows no write area may hold a hidden path.
    if cfg!(windows) {
        assert_eq!(code(&output), "sandbox_failed", "{output:?}");
    } else {
        assert!(output.status.success(), "{output:?}");
    }
    assert_eq!(stdout, "", "{output:?}");
    for (path, data) in [
        ("area/nest/peer/key", "PRIVATE KEY"),
        ("area/conf/.netrc", "password hunter2"),
    ] {
        assert_eq!(fs::read_to_string(real.join(path)).unwrap(), data);
    }
    for path in ["stolen-key", "stolen-netrc", "moved", "moved-conf"] {
        assert!(!real.join("area").join(path).exists(), "{path}");
    }
}

#[test]
fn a_share_inside_a_hidden_path_stays_hidden() {
    let dir = setup();
    let real = portable::real_path(dir.path());
    fs::create_dir_all(real.join("outside/.aws/cli/cache")).unwrap();
    fs::write(real.join("outside/.aws/cli/cache/token"), "SECRET TOKEN").unwrap();
    if cfg!(windows) {
        portable::make_private(&real.join("outside/.aws"));
    }
    let flags = [
        "--area",
        "../outside/.aws/cli/cache",
        "--hidden",
        "../outside/.aws",
    ];
    let read = worker(
        dir.path(),
        &flags,
        &["read"],
        "../outside/.aws/cli/cache/token",
        b"",
    );
    assert!(
        !String::from_utf8_lossy(&read.stdout).contains("SECRET"),
        "{read:?}"
    );
    assert!(!read.status.success(), "{read:?}");
}

#[test]
fn an_opt_in_in_the_system_temp_folder_is_readable_by_exec() {
    let dir = setup();
    let reference = tempfile::tempdir().unwrap();
    let reference = portable::real_path(reference.path());
    fs::write(reference.join("notes.txt"), "reference notes").unwrap();
    let output = exec_script(
        dir.path(),
        &["--read", reference.to_str().unwrap()],
        &format!("cat '{}/notes.txt'", reference.display()),
    );
    let expected: &[u8] = if cfg!(windows) {
        b""
    } else {
        b"reference notes"
    };
    assert_eq!(output.stdout, expected, "{output:?}");
}

#[cfg(target_os = "macos")]
#[test]
fn exec_cannot_read_preferences_through_the_preferences_daemon() {
    // `defaults` asks cfprefsd, not the filesystem, for the host user's
    // preferences: only a sandbox that refuses cfprefsd keeps them out.
    let outside = Command::new("/usr/bin/defaults")
        .args(["read", "-g"])
        .output()
        .unwrap();
    assert!(outside.status.success(), "{outside:?}");
    let dir = setup();
    let inside = exec_script(dir.path(), &[], "/usr/bin/defaults read -g");
    assert!(!inside.status.success(), "{inside:?}");
    assert!(inside.stdout.is_empty(), "{inside:?}");
}

#[cfg(target_os = "linux")]
#[test]
fn an_opt_in_of_run_or_tmp_still_reports_the_programs_exit() {
    let dir = setup();
    for opt_in in ["/run", "/tmp"] {
        if !Path::new(opt_in).is_dir() {
            continue;
        }
        let output = exec_script(dir.path(), &["--read", opt_in], "echo ran; exit 3");
        assert_eq!(output.status.code(), Some(3), "{opt_in}: {output:?}");
        assert_eq!(output.stdout, b"ran\n", "{opt_in}: {output:?}");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn exec_can_make_temp_files_when_an_opt_in_covers_tmp() {
    let dir = setup();
    let script = r#"f=$(mktemp) && echo made > "$f" && cat "$f""#;
    let output = exec_script(dir.path(), &["--read", "/tmp"], script);
    assert_eq!(output.stdout, b"made\n", "{output:?}");
}

/// A home-readable exec with explicit writable roots, using real sandbox code.
#[cfg(unix)]
fn declared_exec(dir: &Path, network: bool, script: &str) -> Output {
    let home = dir.join("area").canonicalize().unwrap();
    // Resolve macOS's Python shim before confinement; xcrun's own cache is
    // unrelated to the command's private TMPDIR.
    let python = Command::new("python3")
        .args(["-c", "import sys; print(sys.executable)"])
        .output()
        .unwrap();
    assert!(python.status.success());
    let python = std::path::PathBuf::from(String::from_utf8(python.stdout).unwrap().trim());
    let path = std::env::join_paths(
        python
            .parent()
            .into_iter()
            .chain(HOST_PATH.split(':').map(Path::new)),
    )
    .unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_latch"));
    command
        .args(["worker", "--home"])
        .arg(&home)
        .arg("--read")
        .arg(&home)
        .arg("--area")
        .arg(home.join("project"))
        .arg("--denied")
        .arg(home.join(".latch"))
        .arg("--hidden")
        .arg(home.join(".latch"))
        .arg("--tmpdir")
        .arg(dir.join("tmp").canonicalize().unwrap());
    if cfg!(target_os = "macos") {
        command.arg("--hidden").arg(home.join("Library"));
        if home.join("Library/Caches").is_dir() {
            command.arg("--area").arg(home.join("Library/Caches"));
        }
    }
    if network {
        command.arg("--network");
    }
    exec_worker(
        command
            .args(["exec", "--", "/bin/sh", "-c", script])
            .current_dir(&home)
            .env_clear()
            .env("PATH", path)
            .env("HOME", &home)
            .env("TMPDIR", dir.join("tmp").canonicalize().unwrap()),
    )
}

#[cfg(unix)]
fn declared_setup() -> tempfile::TempDir {
    let dir = setup();
    for name in ["project", ".ssh", ".latch", "Library/Caches"] {
        fs::create_dir_all(dir.path().join("area").join(name)).unwrap();
    }
    fs::write(dir.path().join("area/.ssh/config"), "readable-dotfile").unwrap();
    fs::write(dir.path().join("area/.latch/private"), "hidden-state").unwrap();
    fs::write(dir.path().join("area/Library/private"), "hidden-library").unwrap();
    dir
}

// Exec reads home on macOS and Linux; a Windows exec reads only its grants
// (src/sandbox/windows.rs's tests).
#[cfg(unix)]
#[test]
fn declared_exec_reads_home_and_confines_writes_and_hidden_data() {
    let dir = declared_setup();
    let output = declared_exec(
        dir.path(),
        false,
        r#"
        cat .ssh/config
        cat .latch/private 2>/dev/null
        cat Library/private 2>/dev/null
        echo denied > undeclared
        echo denied > .latch/undeclared
        echo allowed > project/result
        echo cache > Library/Caches/result
        true
    "#,
    );
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("readable-dotfile"));
    assert!(!stdout.contains("hidden-state"));
    if cfg!(target_os = "macos") {
        assert!(!stdout.contains("hidden-library"));
        assert_eq!(
            fs::read(dir.path().join("area/Library/Caches/result")).unwrap(),
            b"cache\n"
        );
    }
    assert!(!dir.path().join("area/undeclared").exists());
    assert!(!dir.path().join("area/.latch/undeclared").exists());
    assert_eq!(
        fs::read(dir.path().join("area/project/result")).unwrap(),
        b"allowed\n"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn exec_runs_without_a_library_cache_and_does_not_follow_a_cache_alias() {
    let dir = declared_setup();
    let caches = dir.path().join("area/Library/Caches");
    fs::remove_dir(&caches).unwrap();
    let run = || {
        declared_exec(
            dir.path(),
            false,
            "cat .ssh/config; cat Library/private Library/Caches/Library/private 2>/dev/null; true",
        )
    };
    let output = run();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"readable-dotfile");
    assert!(!caches.exists());
    std::os::unix::fs::symlink("..", &caches).unwrap();
    let output = run();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"readable-dotfile");
}

// A host Unix socket, and python3 and node from the system, which a Windows
// container reaches only in Program Files.
#[cfg(unix)]
#[test]
fn declared_exec_needs_network_and_cannot_use_a_host_unix_socket() {
    use std::io::Read;
    use std::net::TcpListener;
    use std::os::unix::net::UnixListener;
    let dir = declared_setup();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let script = format!("curl -fsS --connect-timeout 1 --max-time 2 http://{address}/");
    let offline = declared_exec(dir.path(), false, &script);
    assert!(!offline.status.success(), "offline connected: {offline:?}");
    let serving = std::thread::spawn(move || {
        let (mut connection, _) = listener.accept().unwrap();
        let mut request = [0; 2048];
        assert!(connection.read(&mut request).unwrap() > 0);
        connection
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\nnetwork")
            .unwrap();
    });
    let online = declared_exec(dir.path(), true, &script);
    assert!(online.status.success(), "online: {online:?}");
    assert_eq!(online.stdout, b"network");
    serving.join().unwrap();
    let path = dir.path().join("area/daemon.sock");
    let unix = UnixListener::bind(&path).unwrap();
    unix.set_nonblocking(true).unwrap();
    let script = format!(
        "curl -fsS --max-time 1 --unix-socket '{}' http://localhost/",
        path.display()
    );
    for network in [false, true] {
        let output = declared_exec(dir.path(), network, &script);
        assert!(
            !output.status.success(),
            "Unix socket connected: {output:?}"
        );
        assert_eq!(
            unix.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn declared_exec_cannot_read_its_hosts_process_arguments() {
    let dir = declared_setup();
    let script = format!("test ! -r /proc/{}/cmdline", std::process::id());
    for network in [false, true] {
        let output = declared_exec(dir.path(), network, &script);
        assert!(output.status.success(), "{output:?}");
    }
}

// python3 from the system (see above).
#[cfg(unix)]
#[test]
fn a_networked_exec_resolves_a_hostname() {
    let dir = declared_setup();
    let output = declared_exec(
        dir.path(),
        true,
        "python3 -c 'import socket, sys; assert socket.getaddrinfo(\"localhost\", 80); s=socket.socket(socket.AF_UNIX) if sys.platform == \"darwin\" else None; s.connect(\"/private/var/run/mDNSResponder\") if s else None; print(\"hostname resolved\")'",
    );
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"hostname resolved\n");
}

// python3 and node from the system (see above).
#[cfg(unix)]
#[test]
fn an_offline_exec_supports_unnamed_unix_sockets_and_asyncio() {
    let dir = declared_setup();
    let output = declared_exec(
        dir.path(),
        false,
        r#"python3 -c '
import asyncio, socket
left, right = socket.socketpair()
with left, right:
    left.sendall(b"IPC")
    assert right.recv(3) == b"IPC"
async def main():
    await asyncio.sleep(0)
asyncio.run(main())
print("offline IPC works")
' && node -e 'require("child_process").execFileSync("/bin/echo", ["child process works"], {stdio: "pipe"})'"#,
    );
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"offline IPC works\n");
}

// python3 from the system, and Unix sockets (see above).
#[cfg(unix)]
#[test]
fn a_networked_exec_can_use_unix_sockets_in_its_own_temp() {
    let dir = declared_setup();
    let output = declared_exec(
        dir.path(),
        true,
        r#"python3 -c '
import os, socket
path = os.environ["TMPDIR"] + "/ipc.sock"
with socket.socket(socket.AF_UNIX) as server, socket.socket(socket.AF_UNIX) as client:
    server.bind(path)
    server.listen(1)
    client.connect(path)
    peer, _ = server.accept()
    with peer:
        client.sendall(b"private IPC")
        assert peer.recv(32) == b"private IPC"
print("private temp socket works")
'"#,
    );
    #[cfg(target_os = "linux")]
    {
        // SAFETY: this version query takes null/zero arguments and writes no memory.
        let abi = unsafe { libc::syscall(libc::SYS_landlock_create_ruleset, 0, 0, 1) };
        if abi < 9 {
            assert!(!output.status.success(), "{output:?}");
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("PermissionError"),
                "{output:?}"
            );
            return;
        }
    }
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"private temp socket works\n");
}

#[cfg(target_os = "linux")]
#[test]
fn host_datagram_and_abstract_unix_sockets_are_blocked() {
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::net::{SocketAddr, UnixDatagram, UnixListener};
    let dir = declared_setup();
    let path = dir.path().join("area/datagram.sock");
    let datagram = UnixDatagram::bind(&path).unwrap();
    datagram.set_nonblocking(true).unwrap();
    let name = format!(
        "latch-ipc-test-{}",
        dir.path().file_name().unwrap().to_str().unwrap()
    );
    let abstract_socket =
        UnixListener::bind_addr(&SocketAddr::from_abstract_name(name.as_bytes()).unwrap()).unwrap();
    abstract_socket.set_nonblocking(true).unwrap();
    let path = path.canonicalize().unwrap();
    for network in [false, true] {
        for (kind, address) in [
            ("SOCK_DGRAM", format!("b'{}'", path.display())),
            ("SOCK_STREAM", format!("b'\\0{name}'")),
        ] {
            let send = if kind == "SOCK_DGRAM" {
                format!("s.sendto(b'blocked', {address})")
            } else {
                format!("s.connect({address})")
            };
            let script = format!(
                "python3 -c \"import errno, socket
try:
    s=socket.socket(socket.AF_UNIX, socket.{kind})
    {send}
except OSError as e:
    assert e.errno in (errno.EACCES, errno.EPERM, errno.ECONNREFUSED), e
    print('host blocked')
else:
    raise AssertionError('reached host')\""
            );
            let output = declared_exec(dir.path(), network, &script);
            assert!(output.status.success(), "{kind}: {output:?}");
            assert_eq!(output.stdout, b"host blocked\n");
        }
        let script = format!(
            "python3 -c \"import errno, socket
try:
    left, right = socket.socketpair(socket.AF_UNIX, socket.SOCK_DGRAM)
    with left, right:
        left.sendto(b'blocked', b'{}')
except OSError as e:
    assert e.errno in (errno.EACCES, errno.EPERM), e
    print('datagram relay blocked')
else:
    raise AssertionError('reached host via datagram pair')\"",
            path.display()
        );
        let output = declared_exec(dir.path(), network, &script);
        assert!(output.status.success(), "{output:?}");
        assert_eq!(output.stdout, b"datagram relay blocked\n");
        let script = format!(
            "python3 -c \"import errno, socket
for kind in (socket.SOCK_STREAM, socket.SOCK_SEQPACKET):
    left, right = socket.socketpair(socket.AF_UNIX, kind)
    with left, right:
        try:
            left.connect(b'{}')
        except OSError:
            pass
        else:
            raise AssertionError('redirected a connected pair')
        try:
            left.sendto(b'own pair', b'{}')
        except OSError as e:
            assert e.errno == errno.EISCONN, e
            left.sendall(b'own pair')
        assert right.recv(32) == b'own pair'
print('connected pairs stay private')\"",
            path.display(),
            path.display()
        );
        let output = declared_exec(dir.path(), network, &script);
        assert!(output.status.success(), "{output:?}");
        assert_eq!(output.stdout, b"connected pairs stay private\n");
        assert_eq!(
            datagram.recv(&mut [0]).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        assert_eq!(
            abstract_socket.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn an_exec_worker_cannot_connect_to_sockets_in_the_host_state() {
    use std::os::unix::net::UnixListener;

    // macOS Unix socket addresses must fit even when the checkout path is long.
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let root = dir.path().canonicalize().unwrap();
    for folder in ["area", "state", "tmp"] {
        fs::create_dir(root.join(folder)).unwrap();
    }
    for (state, alias) in [
        ("state", "area/state-link"),
        ("area/peer", "area/peer-link"),
    ] {
        fs::create_dir_all(root.join(state).join("nested")).unwrap();
        std::os::unix::fs::symlink(root.join(state), root.join(alias)).unwrap();
    }
    let program = root.join("area/connect");
    let compiled = Command::new("rustc")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/unix-connect.rs"))
        .arg("-o")
        .arg(&program)
        .output()
        .unwrap();
    assert!(compiled.status.success(), "{compiled:?}");
    let allowed = root.join("area/allowed.sock");
    let _allowed = UnixListener::bind(&allowed).unwrap();
    let run = |socket: &Path| {
        exec_worker(
            Command::new(env!("CARGO_BIN_EXE_latch"))
                .args([
                    "worker", "--area", "..", "--denied", "../state", "--hidden", "peer",
                    "--tmpdir", "../tmp", "exec", "--",
                ])
                .arg(&program)
                .arg(socket)
                .current_dir(root.join("area"))
                .env_clear(),
        )
    };
    // Host Unix sockets stay blocked even outside state.
    let outside = run(&allowed);
    assert!(!outside.status.success(), "{outside:?}");
    for (state, alias) in [
        ("state", "area/state-link"),
        ("area/peer", "area/peer-link"),
    ] {
        for relative in ["serve-v4.sock", "nested/other.sock"] {
            let socket = root.join(state).join(relative);
            let listener = UnixListener::bind(&socket).unwrap();
            assert!(
                Command::new(&program)
                    .arg(&socket)
                    .status()
                    .unwrap()
                    .success()
            );
            for path in [socket, root.join(alias).join(relative)] {
                let denied = run(&path);
                assert!(
                    !denied.status.success(),
                    "worker connected to {}: {denied:?}",
                    path.display()
                );
                let stderr = String::from_utf8_lossy(&denied.stderr);
                assert!(
                    stderr.contains("PermissionDenied")
                        || (cfg!(target_os = "linux") && stderr.contains("NotFound")),
                    "{denied:?}"
                );
            }
            drop(listener);
        }
    }
}

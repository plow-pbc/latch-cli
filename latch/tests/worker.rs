//! Runs the real `latch worker` against a throwaway area and state
//! directory, to check what the sandbox lets it touch.

use std::fs;
use std::io::Write;
#[cfg(target_os = "linux")]
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Output, Stdio};

use serde_json::Value;

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
    let mut child = Command::new(env!("CARGO_BIN_EXE_latch"))
        .args(["worker", "--area", "../area", "--denied", "../state"])
        .args(extra)
        .args(action)
        .args(["--", path])
        .current_dir(dir.join("area"))
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // A worker that refuses exits without reading its stdin; that is fine.
    let _ = child.stdin.take().unwrap().write_all(stdin);
    child.wait_with_output().unwrap()
}

/// The code a worker reports for a path the sandbox keeps from it: Linux
/// leaves it out of the worker's view, macOS refuses it.
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

#[cfg(any(target_os = "macos", target_os = "linux"))]
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

    // The state folder, directly or through a link, stays hidden.
    for path in ["link", "../state/secret"] {
        let refused = worker(dir.path(), &[], &["read"], path, b"");
        assert_eq!(code(&refused), hidden_code(), "reading {path}");
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

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn missing_files_and_hidden_paths_report_the_operating_system_error() {
    let dir = setup();
    let root = dir.path().canonicalize().unwrap();
    std::os::unix::fs::symlink(root.join("outside/missing"), root.join("area/dangling")).unwrap();
    std::os::unix::fs::symlink(root.join("area/missing"), root.join("area/inside-link")).unwrap();
    for path in ["missing", "inside-link", "missing-dir/file"] {
        let output = worker(dir.path(), &[], &["read"], path, b"");
        assert_eq!(code(&output), "not_found", "{path}");
    }
    for path in ["../outside/missing", "dangling", "../state/missing"] {
        let output = worker(dir.path(), &[], &["read"], path, b"");
        assert_eq!(code(&output), "not_found", "{path}");
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_write_is_all_or_nothing() {
    let dir = setup();
    let area = dir.path().join("area");
    fs::write(area.join("keep.txt"), "old contents").unwrap();

    // The transfer breaks off: 3 of the 10 promised bytes arrive.
    let short = worker(
        dir.path(),
        &[],
        &["write", "--size", "10"],
        "keep.txt",
        b"new",
    );
    assert_eq!(code(&short), "transfer_incomplete");
    assert_eq!(fs::read(area.join("keep.txt")).unwrap(), b"old contents");

    // All bytes arrive: the new contents replace the old in one step.
    let full = worker(
        dir.path(),
        &[],
        &["write", "--size", "12"],
        "keep.txt",
        b"new contents",
    );
    assert!(full.status.success());
    assert_eq!(fs::read(area.join("keep.txt")).unwrap(), b"new contents");

    // No temporary file is left behind either way.
    let mut names: Vec<String> = fs::read_dir(&area)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["inside.txt", "keep.txt", "link"]);
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn an_exec_worker_runs_programs_inside_the_sandbox() {
    let dir = setup();
    let exec = |argv: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_latch"))
            .args([
                "worker", "--area", "../area", "--denied", "../state", "--tmpdir", "../tmp",
                "exec", "--",
            ])
            .args(argv)
            .current_dir(dir.path().join("area"))
            .env_clear()
            .output()
            .unwrap()
    };
    let pipe = exec(&["/bin/sh", "-c", "echo hello | tr a-z A-Z"]);
    assert!(pipe.status.success(), "{pipe:?}");
    assert_eq!(pipe.stdout, b"HELLO\n");

    let state = exec(&["/bin/sh", "-c", "cat ../state/secret"]);
    assert_eq!(state.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&state.stderr).contains(if cfg!(target_os = "macos") {
            "Operation not permitted"
        } else {
            "No such file or directory"
        })
    );
    assert!(state.stdout.is_empty());

    let outside = exec(&["/bin/sh", "-c", "echo x > ../outside/new.txt"]);
    assert!(!outside.status.success());
    assert!(!dir.path().join("outside/new.txt").exists());
}

#[cfg(target_os = "macos")]
#[test]
fn an_exec_cannot_kill_its_supervisor() {
    let dir = setup();
    let output = Command::new(env!("CARGO_BIN_EXE_latch"))
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
        .env_clear()
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"protected\n", "{output:?}");
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn an_agent_can_neither_read_nor_change_the_owners_rules() {
    let dir = setup();
    let rules = dir.path().join("state/rules");
    fs::create_dir(&rules).unwrap();
    fs::write(rules.join("host.txt"), "Never send email.").unwrap();
    let exec = |script: &str| {
        Command::new(env!("CARGO_BIN_EXE_latch"))
            .args([
                "worker", "--area", "..", "--denied", "../state", "--tmpdir", "../tmp", "exec",
                "--", "/bin/sh", "-c", script,
            ])
            .current_dir(dir.path().join("area"))
            .env_clear()
            .output()
            .unwrap()
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
    }
    let read = worker(
        dir.path(),
        &["--area", ".."],
        &["read"],
        "../state/rules/host.txt",
        b"",
    );
    assert!(
        ["not_found", "sandbox_denied"].contains(&code(&read).as_str()),
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

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_workdir_outside_the_areas_is_refused() {
    let dir = setup();
    let real = dir.path().canonicalize().unwrap();
    let area = real.join("area");
    for workdir in [real.join("outside"), real.join("state")] {
        let output = Command::new(env!("CARGO_BIN_EXE_latch"))
            .args(["worker", "--area"])
            .arg(&area)
            .arg("--denied")
            .arg(real.join("state"))
            .arg("--tmpdir")
            .arg(real.join("tmp"))
            .args(["exec", "--workdir"])
            .arg(&workdir)
            .args(["--", "/bin/pwd"])
            .current_dir(&area)
            .env_clear()
            .output()
            .unwrap();
        assert!(output.stdout.is_empty());
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

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
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
    for path in ["../state/secret", "link"] {
        let hidden = worker(dir.path(), &["--area", ".."], &["read"], path, b"");
        assert_eq!(code(&hidden), "not_found", "{path}");
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

/// `latch serve <area>` with no login, run with a seccomp filter
/// (installed in the child before it starts) that makes `syscall` fail with
/// EPERM when `rules` match. Without the filter it stops at the login check.
#[cfg(target_os = "linux")]
fn serve_with_denied(rules: Option<(i64, Vec<seccompiler::SeccompRule>)>) -> Output {
    use std::os::unix::process::CommandExt;

    let dir = setup();
    let area = dir.path().join("area").canonicalize().unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_latch"));
    command
        .args(["serve", "--no-dashboard", "--state-dir"])
        .arg(dir.path().join("state"))
        .arg(area)
        .env_clear();
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
        let output = Command::new(env!("CARGO_BIN_EXE_latch"))
            .args([
                "worker", "--area", "../area", "--denied", "../state", "--tmpdir", "../tmp",
                "exec", "--", "/bin/sh", "-c", script,
            ])
            .current_dir(dir.path().join("area"))
            .env_clear()
            .output()
            .unwrap();
        assert_eq!(output.status.code(), code, "{output:?}");
        assert_eq!(output.status.signal(), signal, "{output:?}");
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn an_exec_worker_writes_only_its_own_temp_folder_outside_the_areas() {
    let dir = setup();
    let real = dir.path().canonicalize().unwrap();
    // Two per-call folders in the OS temp folder, as a host makes them.
    let mine = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    // The refused write is in a subshell: how a shell exits when a
    // redirection fails differs (bash 1, dash 2).
    let script = r#"echo x > "$TMPDIR/t" && cat "$TMPDIR/t"; (echo y > "$0/stolen") 2>/dev/null && echo stolen; exit 0"#;
    let output = Command::new(env!("CARGO_BIN_EXE_latch"))
        .arg("worker")
        .arg("--area")
        .arg(real.join("area"))
        .arg("--denied")
        .arg(real.join("state"))
        .arg("--tmpdir")
        .arg(mine.path().canonicalize().unwrap())
        .args(["exec", "--", "/bin/sh", "-c", script])
        .arg(other.path().canonicalize().unwrap())
        .current_dir(real.join("area"))
        .env_clear()
        .env("TMPDIR", mine.path().canonicalize().unwrap())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"x\n", "{output:?}");
    if cfg!(target_os = "macos") {
        // macOS: the program's TMPDIR is the folder it was given, and nothing else of the OS temp folder.
        assert!(mine.path().join("t").exists());
        assert!(!other.path().join("stolen").exists());
    } else {
        // Linux: the private /tmp; the host's folders are not in its view.
        assert!(!mine.path().join("t").exists());
        assert!(!other.path().join("stolen").exists());
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn exec_reads_and_writes_only_areas_never_state_or_latch() {
    // The latch running the worker and the state folder both sit inside the
    // area, as when a shared folder holds them.
    let dir = setup();
    let real = dir.path().canonicalize().unwrap();
    let area = real.join("area");
    fs::create_dir_all(area.join("bin")).unwrap();
    fs::create_dir_all(area.join("keep/.state")).unwrap();
    fs::write(area.join("keep/.state/secret"), "secret").unwrap();
    let latch = area.join("bin/latch");
    fs::copy(env!("CARGO_BIN_EXE_latch"), &latch).unwrap();
    let before = fs::read(&latch).unwrap();
    let script = r#"
        cat ../outside/other.txt && echo " READ-OUTSIDE"
        cat inside.txt && echo " read-inside-ok"
        echo x > ../outside/new.txt && echo "WROTE-OUTSIDE"
        cat keep/.state/secret; echo x > keep/.state/new && echo "WROTE-STATE"
        echo x > bin/latch && echo "WROTE-LATCH"
        rm -f bin/latch && echo "REMOVED-LATCH"
        ln bin/latch hard 2>/dev/null && echo x >> hard && echo "WROTE-HARDLINK"
        mv bin bin2 2>/dev/null && echo "MOVED-LATCH"
        mkdir -p bin && echo planted > bin/latch 2>/dev/null && echo "PLANTED-LATCH"
        mv keep keep2 2>/dev/null && echo "MOVED-STATE" && cat keep2/.state/secret
        mkdir -p keep/.state && echo planted > keep/.state/config.json 2>/dev/null && echo "PLANTED-STATE"
        cat */.state/secret 2>/dev/null
        true
    "#;
    let output = Command::new(&latch)
        .arg("worker")
        .arg("--area")
        .arg(&area)
        .arg("--denied")
        .arg(area.join("keep/.state"))
        .arg("--tmpdir")
        .arg(real.join("tmp"))
        .args(["exec", "--", "/bin/sh", "-c", script])
        .current_dir(&area)
        .env_clear()
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{output:?}");
    assert!(stdout.contains("inside read-inside-ok"), "{stdout}");
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

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_write_to_a_folder_lands_inside_it_under_its_own_name() {
    let dir = setup();
    fs::create_dir(dir.path().join("area/docs")).unwrap();
    let into = worker(
        dir.path(),
        &[],
        &["write", "--size", "2", "--name", "notes.txt"],
        "docs",
        b"hi",
    );
    assert!(into.status.success(), "{into:?}");
    assert!(String::from_utf8_lossy(&into.stdout).contains("\"path\":\"docs/notes.txt\""));
    assert_eq!(
        fs::read(dir.path().join("area/docs/notes.txt")).unwrap(),
        b"hi"
    );
    // A name that is a path of its own is not used.
    let sneaky = worker(
        dir.path(),
        &[],
        &["write", "--size", "1", "--name", "../x", "--"],
        "docs",
        b"x",
    );
    assert!(!sneaky.status.success(), "{sneaky:?}");
    assert!(!dir.path().join("area/x").exists());
}

/// The `PATH` a host gives exec's programs.
const HOST_PATH: &str = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";

/// `latch worker … exec -- /bin/sh -c SCRIPT`, as a host runs it: from the
/// area, with HOME there too, and `extra` worker flags.
fn exec_script(dir: &Path, extra: &[&str], script: &str) -> Output {
    let real = dir.canonicalize().unwrap();
    Command::new(env!("CARGO_BIN_EXE_latch"))
        .args(["worker", "--area", "../area", "--denied", "../state"])
        .args(extra)
        .args(["--tmpdir", "../tmp", "exec", "--", "/bin/sh", "-c", script])
        .current_dir(real.join("area"))
        .env_clear()
        .env("PATH", HOST_PATH)
        .env("HOME", real.join("area"))
        .env("TMPDIR", real.join("tmp"))
        .output()
        .unwrap()
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn everyday_tools_work_reading_only_the_area_and_the_system() {
    let dir = setup();
    let installed = |tool: &str| {
        HOST_PATH
            .split(':')
            .any(|folder| Path::new(folder).join(tool).is_file())
    };
    let battery = [
        ("ls", "ls -la && grep -rn inside . && pwd"),
        (
            "git",
            "git init -q repo && cd repo && echo hi > a && git add a \
             && git -c user.name=t -c user.email=t@example.com commit -qm one \
             && git status --short && git log --oneline && git diff HEAD",
        ),
        (
            "python3",
            "python3 -c 'import ssl, json, sqlite3; print(ssl.OPENSSL_VERSION)'",
        ),
        ("node", "node -e 'console.log(require(\"os\").platform())'"),
    ];
    for (tool, script) in battery {
        // A tool this machine lacks cannot be tried here.
        if !installed(tool) {
            eprintln!("{tool} is not installed here: not tried");
            continue;
        }
        let output = exec_script(dir.path(), &[], script);
        assert!(output.status.success(), "{tool}: {output:?}");
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
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

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn credentials_stay_hidden_inside_an_opt_in_and_inside_the_area() {
    let dir = setup();
    let real = dir.path().canonicalize().unwrap();
    for home in ["outside", "area"] {
        fs::create_dir(real.join(home).join(".ssh")).unwrap();
        fs::write(real.join(home).join(".ssh/id_ed25519"), "PRIVATE KEY").unwrap();
        fs::write(real.join(home).join(".netrc"), "password hunter2").unwrap();
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
    for path in [
        "../outside/.ssh/id_ed25519",
        "../outside/.netrc",
        ".ssh/id_ed25519",
        ".netrc",
    ] {
        let read = worker(dir.path(), &flags, &["read"], path, b"");
        let stdout = String::from_utf8_lossy(&read.stdout);
        assert!(
            !stdout.contains("PRIVATE") && !stdout.contains("hunter2"),
            "{path}: {read:?}"
        );
    }
    let opted_in = worker(dir.path(), &flags, &["read"], "../outside/other.txt", b"");
    assert_eq!(opted_in.stdout, b"{\"size\":5}\nother");

    let script = r#"
        cat ../outside/.ssh/id_ed25519 ../outside/.netrc .ssh/id_ed25519 .netrc
        echo key >> .ssh/authorized_keys && echo "WROTE-SSH"
        mv .ssh moved 2>/dev/null && echo "MOVED-SSH" && cat moved/id_ed25519
        cat inside.txt
        true
    "#;
    let output = exec_script(dir.path(), &flags, script);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(stdout, "inside", "{output:?}");
    assert!(!real.join("area/.ssh/authorized_keys").exists());
    assert_eq!(
        fs::read(real.join("area/.ssh/id_ed25519")).unwrap(),
        b"PRIVATE KEY"
    );
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_hidden_path_can_be_neither_hard_linked_nor_moved_out_of_reach() {
    let dir = setup();
    let real = dir.path().canonicalize().unwrap();
    // A hidden folder and a hidden file, each below a folder of the area.
    fs::create_dir_all(real.join("area/nest/peer")).unwrap();
    fs::write(real.join("area/nest/peer/key"), "PRIVATE KEY").unwrap();
    fs::create_dir_all(real.join("area/conf")).unwrap();
    fs::write(real.join("area/conf/.netrc"), "password hunter2").unwrap();
    let flags = ["--hidden", "nest/peer", "--hidden", "conf/.netrc"];
    let script = r#"
        ln nest/peer/key stolen-key 2>/dev/null && echo "LINKED-KEY"
        ln conf/.netrc stolen-netrc 2>/dev/null && echo "LINKED-NETRC"
        cat stolen-key stolen-netrc 2>/dev/null
        mv nest moved 2>/dev/null && echo "MOVED-NEST"
        mv conf moved-conf 2>/dev/null && echo "MOVED-CONF"
        true
    "#;
    let output = exec_script(dir.path(), &flags, script);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{output:?}");
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

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_share_inside_a_hidden_path_stays_hidden() {
    let dir = setup();
    let real = dir.path().canonicalize().unwrap();
    fs::create_dir_all(real.join("outside/.aws/cli/cache")).unwrap();
    fs::write(real.join("outside/.aws/cli/cache/token"), "SECRET TOKEN").unwrap();
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

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn an_opt_in_in_the_system_temp_folder_is_readable_by_exec() {
    let dir = setup();
    let reference = tempfile::tempdir().unwrap();
    let reference = reference.path().canonicalize().unwrap();
    fs::write(reference.join("notes.txt"), "reference notes").unwrap();
    let output = exec_script(
        dir.path(),
        &["--read", reference.to_str().unwrap()],
        &format!("cat {}/notes.txt", reference.display()),
    );
    assert_eq!(output.stdout, b"reference notes", "{output:?}");
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

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
fn the_sandbox_reads_everywhere_but_the_state_and_writes_only_the_area() {
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

    // Outside the area, reads work, as they do for exec.
    let outside = worker(dir.path(), &[], &["read"], "../outside/other.txt", b"");
    assert!(outside.status.success(), "{outside:?}");
    assert_eq!(outside.stdout, b"{\"size\":5}\nother");

    // The state folder, directly or through a link, stays hidden.
    for path in ["link", "../state/secret"] {
        let refused = worker(dir.path(), &[], &["read"], path, b"");
        assert_eq!(
            code(&refused),
            if cfg!(target_os = "linux") {
                "not_found"
            } else {
                "sandbox_denied"
            },
            "reading {path}"
        );
    }
    // Writes outside the area are refused (Linux: a read-only view).
    let refused = worker(
        dir.path(),
        &[],
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
fn sharing_the_root_folder_allows_writes_but_still_hides_the_state_folder() {
    let dir = setup();
    let real = dir.path().canonicalize().unwrap();
    // A folder on the root filesystem itself, not on a separate mount (the
    // test folder may be one), where any user may write.
    let on_root = tempfile::tempdir_in("/var/tmp").unwrap();
    let on_root = on_root.path().canonicalize().unwrap();
    let root_worker = |args: &[&str], path: &Path, stdin: &[u8]| {
        let mut child = Command::new(env!("CARGO_BIN_EXE_latch"))
            .args(["worker", "--area", "/", "--denied"])
            .arg(real.join("state"))
            .arg("--tmpdir")
            .arg(real.join("tmp"))
            .args(args)
            .arg(path)
            .current_dir(&real)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let _ = child.stdin.take().unwrap().write_all(stdin);
        child.wait_with_output().unwrap()
    };

    let write = root_worker(
        &["write", "--size", "7", "--"],
        &on_root.join("new.txt"),
        b"written",
    );
    assert!(write.status.success(), "{write:?}");
    assert_eq!(fs::read(on_root.join("new.txt")).unwrap(), b"written");
    let exec = root_worker(
        &["exec", "--", "/bin/sh", "-c", "touch \"$0\""],
        &on_root.join("by-exec"),
        b"",
    );
    assert!(exec.status.success(), "{exec:?}");
    assert!(on_root.join("by-exec").exists());

    let secret = root_worker(&["read", "--"], &real.join("state/secret"), b"");
    assert!(!secret.status.success(), "{secret:?}");
    assert!(!String::from_utf8_lossy(&secret.stdout).contains("secret"));
    let into_state = root_worker(
        &["write", "--size", "1", "--"],
        &real.join("state/new"),
        b"x",
    );
    assert!(!into_state.status.success(), "{into_state:?}");
    let by_exec = root_worker(
        &["exec", "--", "/bin/sh", "-c", "touch \"$0\""],
        &real.join("state/by-exec"),
        b"",
    );
    assert!(!by_exec.status.success(), "{by_exec:?}");
    assert!(!real.join("state/new").exists() && !real.join("state/by-exec").exists());
    assert_eq!(fs::read(real.join("state/secret")).unwrap(), b"secret");
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
fn exec_reads_everything_but_writes_only_areas_never_state_or_latch() {
    // The latch running the worker and the state folder both sit inside the
    // area, as with `serve ~`.
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
        cat ../outside/other.txt && echo " read-outside-ok"
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
    assert!(stdout.contains("other read-outside-ok"), "{stdout}");
    for escape in ["WROTE-", "REMOVED-", "MOVED-", "PLANTED-", "secret"] {
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

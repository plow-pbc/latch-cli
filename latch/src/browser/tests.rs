//! A real session on the host: the pinned Chrome for Testing, from the
//! host's browser worker, run from a logged-in desktop session; headless
//! unless built with `headed-tests` (see `HEADLESS`). It
//! needs the engine installed where `just cft` (part of `just test`, and a
//! step in CI) puts it, `.dev/cft-state`, or in the state directory
//! `LATCH_CFT_STATE` names; without it the test fails, saying so. On Linux
//! arm64 the engine runs inside the worker's bubblewrap, and the tests are
//! built only with `linux-browser-tests`, on a host whose install.sh put
//! CfT in place (`just conformance cft`).
//!
//! `LATCH_BROWSER_EVIDENCE=DIR` also keeps the screenshot there.

#[cfg(target_os = "macos")]
use std::ffi::{c_char, c_int};
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use super::host::{self, Browsers};
use super::profile;
use crate::failure::Failure;

/// The engines the conformance checks run on, each its own test:
/// `browser::tests::cft::NAME`, and `browser::tests::camoufox::NAME` on
/// macOS (and the same under `fills` and `steps`). `just conformance
/// ENGINE` runs one engine's.
macro_rules! conformance {
    ($($check:ident),* $(,)?) => {
        mod cft {
            $(#[tokio::test]
            async fn $check() {
                super::$check("cft").await
            })*
        }
        #[cfg(target_os = "macos")]
        mod camoufox {
            $(#[tokio::test]
            async fn $check() {
                super::$check("camoufox").await
            })*
        }
    };
}
mod egress;
mod fills;
mod gated;
mod goals;
mod handoff;
mod shop;
mod steps;
use super::session::Session;

/// The caller every host-level test calls as.
const CALLER: &str = "test-caller";

/// What a sandboxed `cat` or `ls` of a hidden path says: Seatbelt refuses
/// it; Linux's bubblewrap leaves the path out.
const HIDDEN: &str = if cfg!(target_os = "macos") {
    "Operation not permitted"
} else {
    "No such file or directory"
};

/// The engine runs with no window, so a test run never puts a browser over
/// the owner's screen. `just test-headed` (the `headed-tests` feature) runs
/// the same tests headed, with a real window behind the owner's.
const HEADLESS: bool = !cfg!(feature = "headed-tests");

#[cfg(target_os = "macos")]
unsafe extern "C" {
    /// macOS: 1 if the process is inside a Seatbelt sandbox (with no
    /// operation named), 0 if not.
    fn sandbox_check(pid: libc::pid_t, operation: *const c_char, kind: c_int, ...) -> c_int;
}

#[cfg(target_os = "macos")]
fn sandboxed(pid: u32) -> bool {
    // SAFETY: no operation or filter arguments; reads the pid's sandbox state.
    unsafe { sandbox_check(pid as libc::pid_t, std::ptr::null(), 0) == 1 }
}

/// Linux: the user namespace process `pid` is in, and its seccomp mode
/// (2: a filter is on).
#[cfg(target_os = "linux")]
fn confinement(pid: u32) -> (String, String) {
    let namespace = fs::read_link(format!("/proc/{pid}/ns/user"))
        .map(|link| link.display().to_string())
        .unwrap_or_default();
    let status = fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default();
    let seccomp = status
        .lines()
        .find_map(|line| line.strip_prefix("Seccomp:"))
        .unwrap_or_default()
        .trim()
        .to_string();
    (namespace, seccomp)
}

struct Process {
    pid: u32,
    pgid: u32,
    args: String,
}

fn processes() -> Vec<Process> {
    let out = Command::new("/bin/ps")
        .args(["-Aww", "-o", "pid=,pgid=,args="])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let pgid = fields.next()?.parse().ok()?;
            Some(Process {
                pid,
                pgid,
                args: fields.collect::<Vec<_>>().join(" "),
            })
        })
        .collect()
}

/// The engine's pid as the host sees it. On Linux the worker reports its
/// pid inside the worker's bubblewrap, so it is found by its profile
/// argument instead.
fn host_pid(pid: u32, profile_arg: &str) -> u32 {
    if cfg!(target_os = "macos") {
        return pid;
    }
    processes()
        .into_iter()
        .find(|p| p.args.contains(profile_arg) && !p.args.contains("--type="))
        .unwrap_or_else(|| panic!("no browser process with {profile_arg}"))
        .pid
}

/// The names of process `pid`'s environment variables (`args`: its
/// arguments, which ps prints the environment after on macOS).
fn environment(pid: u32, args: &str) -> Vec<String> {
    let text = if cfg!(target_os = "macos") {
        let with_env = Command::new("/bin/ps")
            .args(["-wwE", "-o", "command=", "-p", &pid.to_string()])
            .output()
            .unwrap();
        let with_env = String::from_utf8_lossy(&with_env.stdout).into_owned();
        with_env.trim().strip_prefix(args).unwrap().to_string()
    } else {
        String::from_utf8_lossy(&fs::read(format!("/proc/{pid}/environ")).unwrap())
            .replace('\0', " ")
    };
    text.split(' ')
        .filter_map(|v| v.split_once('='))
        .map(|v| v.0.to_string())
        .filter(|name| {
            !name.is_empty()
                && name
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        })
        .collect()
}

/// How many TCP listeners `exec` sees on this machine, and which of
/// `pids` hold one: from exec's lsof on macOS; on Linux from exec's
/// `/proc/net`, matched to the sockets each pid holds.
fn tcp_listeners(state: &Path, area: &Path, pids: &[u32]) -> (usize, Vec<u32>) {
    if cfg!(target_os = "macos") {
        let (_, listening) = exec(state, area, "lsof -nP -iTCP -sTCP:LISTEN -F p");
        let listeners: Vec<u32> = listening
            .lines()
            .filter_map(|line| line.strip_prefix('p')?.parse().ok())
            .collect();
        let held = pids
            .iter()
            .copied()
            .filter(|p| listeners.contains(p))
            .collect();
        return (listeners.len(), held);
    }
    let (_, tables) = exec(state, area, "cat /proc/net/tcp /proc/net/tcp6");
    // Each socket's line: state (0A: listening) is the 4th field, its inode the 10th.
    let listening: Vec<String> = tables
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>())
        .filter(|fields| fields.len() > 9 && fields[3] == "0A")
        .map(|fields| format!("socket:[{}]", fields[9]))
        .collect();
    let held = pids
        .iter()
        .copied()
        .filter(|pid| {
            fs::read_dir(format!("/proc/{pid}/fd"))
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|fd| fs::read_link(fd.path()).ok())
                .any(|link| listening.contains(&link.display().to_string()))
        })
        .collect();
    (listening.len(), held)
}

/// The parent of process `pid`.
fn helper_of(pid: u32) -> u32 {
    let out = Command::new("/bin/ps")
        .args(["-o", "ppid=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
}

/// Serves `pages` (path, HTML) on 127.0.0.1 for as long as the test runs;
/// returns the port. Each connection gets its own thread: a browser opens
/// connections it may not use at once.
fn serve(pages: Vec<(String, Vec<u8>)>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let pages = Arc::new(pages);
    std::thread::spawn(move || {
        for socket in listener.incoming() {
            let Ok(mut socket) = socket else { continue };
            let pages = pages.clone();
            std::thread::spawn(move || {
                let mut request = [0u8; 4096];
                let n = socket.read(&mut request).unwrap_or(0);
                let line = String::from_utf8_lossy(&request[..n]);
                let path = line.split(' ').nth(1).unwrap_or("/");
                let path = path.split('?').next().unwrap_or("/");
                let found = pages.iter().find(|(p, _)| p == path);
                let (status, body) = match found {
                    Some((_, body)) => ("200 OK", body.as_slice()),
                    None => ("404 Not Found", &b""[..]),
                };
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = socket.write_all(head.as_bytes());
                let _ = socket.write_all(body);
            });
        }
    });
    port
}

/// A local page on 127.0.0.1.
fn local_page() -> String {
    let page = b"<!doctype html><title>latch local page</title>\
        <body style=\"margin:0;background:#1d6f42;color:#fff;font:48px sans-serif\">\
        <h1 style=\"margin:40px\">latch browser: local page</h1></body>";
    format!(
        "http://127.0.0.1:{}/",
        serve(vec![("/".into(), page.to_vec())])
    )
}

/// The test site in `tests/browser-site`, on two origins: 127.0.0.1 for
/// the pages, localhost (another site) for the cross-site frame. Returns
/// the frames page's URL.
fn test_site() -> String {
    let folder = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/browser-site");
    let pages = || {
        fs::read_dir(&folder)
            .unwrap()
            .map(|entry| {
                let path = entry.unwrap().path();
                let name = format!("/{}", path.file_name().unwrap().to_string_lossy());
                (name, fs::read(path).unwrap())
            })
            .collect::<Vec<_>>()
    };
    let pages_port = serve(pages());
    let cross_port = serve(pages());
    format!("http://127.0.0.1:{pages_port}/frames.html?cross={cross_port}")
}

/// The latch binary cargo builds beside this test binary.
fn latch() -> PathBuf {
    let deps = std::env::current_exe().unwrap();
    deps.parent().unwrap().parent().unwrap().join("latch")
}

/// Runs `sh -c script` as the host's `exec` does: a sandboxed exec worker
/// with one area, the state directory denied.
fn exec(state: &Path, area: &Path, script: &str) -> (bool, String) {
    let tmp = tempfile::tempdir_in(area).unwrap();
    let out = Command::new(latch())
        .args(["worker", "--area"])
        .arg(area)
        .arg("--denied")
        .arg(state)
        .arg("--tmpdir")
        .arg(tmp.path())
        .args(["exec", "--", "/bin/sh", "-c", script])
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("HOME", area)
        .current_dir(area)
        .output()
        .unwrap();
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    (out.status.success(), text)
}

conformance!(
    the_engine_runs_from_the_host_with_its_own_sandbox_and_leaves_nothing,
    the_core_reads_frames_and_acts_by_ref_with_trusted_input,
    frames_that_come_and_go_while_a_page_loads_break_no_call,
    a_frames_target_under_a_parent_page_layer_is_refused,
    eval_upload_and_download_move_values_and_files,
    sessions_on_one_profile_share_logins_and_nothing_else,
    sessions_warn_on_a_shared_site_and_a_reserved_one_is_refused,
    navigating_into_a_reserved_site_is_refused_before_it_is_sent,
    one_sessions_wait_does_not_hold_the_browser_for_another,
    a_call_longer_than_the_idle_timeout_does_not_end_its_session,
    a_browser_that_leaves_calls_unanswered_is_closed,
    a_dialog_that_opens_between_calls_is_refused_at_once,
    a_dialog_in_one_session_does_not_hold_another_sessions_download,
    a_download_heard_before_another_sessions_dialog_does_not_wait_on_it,
    back_into_a_reserved_site_is_refused_after_a_return_visit,
    an_answer_shows_the_screen_and_observe_all_every_element,
);

/// At most this many tests drive a real engine at once: two engines' worth
/// of browsers, all at once, starve each other and time out.
const ENGINES_AT_ONCE: usize = 6;
static RUNNING: std::sync::Mutex<usize> = std::sync::Mutex::new(0);
static FREED: std::sync::Condvar = std::sync::Condvar::new();

/// A test's folder, and its turn at the engines, both given back on drop.
struct TestDir {
    dir: tempfile::TempDir,
    _turn: Turn,
}

impl std::ops::Deref for TestDir {
    type Target = tempfile::TempDir;
    fn deref(&self) -> &Self::Target {
        &self.dir
    }
}

/// A test that ends with a browser still running in its folder lets that
/// browser write there after the folder is gone: the test must close its
/// sessions first. A test already failing is not checked again.
impl Drop for TestDir {
    fn drop(&mut self) {
        if std::thread::panicking() {
            return;
        }
        let out = Command::new("ps")
            .args(["-axww", "-o", "command="])
            .output()
            .unwrap();
        // By its unique name: processes see the folder by another spelling.
        let folder = self
            .dir
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let left: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|line| line.contains(&folder))
            .map(|line| line.chars().take(160).collect())
            .collect();
        assert!(
            left.is_empty(),
            "still running in the test's folder (close its sessions first): {left:#?}"
        );
    }
}

/// `n` of the `ENGINES_AT_ONCE` places.
struct Turn(usize);

impl Turn {
    fn wait(n: usize) -> Turn {
        let mut running = RUNNING.lock().unwrap();
        while *running + n > ENGINES_AT_ONCE {
            running = FREED.wait(running).unwrap();
        }
        *running += n;
        Turn(n)
    }
}

impl Drop for Turn {
    fn drop(&mut self) {
        *RUNNING.lock().unwrap() -= self.0;
        FREED.notify_all();
    }
}

/// A fresh state directory for one test, sharing only the installed
/// plugins, with `profiles` made for `engine`, and a folder to share with
/// `exec`. The engine must be installed (see the top of this file). Waits
/// for its turn at the engines (`ENGINES_AT_ONCE`).
fn test_state(engine: &str, profiles: &[&str]) -> (TestDir, PathBuf, PathBuf) {
    test_state_taking(engine, profiles, 1)
}

/// `test_state` for a test whose page keeps the browser busy (frames
/// coming and going all the time): it runs with no other engine test.
fn test_state_alone(engine: &str, profiles: &[&str]) -> (TestDir, PathBuf, PathBuf) {
    test_state_taking(engine, profiles, ENGINES_AT_ONCE)
}

fn test_state_taking(
    engine: &str,
    profiles: &[&str],
    places: usize,
) -> (TestDir, PathBuf, PathBuf) {
    let installed = std::env::var_os("LATCH_CFT_STATE")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../.dev/cft-state"));
    let (version, engine_folder) = match engine {
        "camoufox" => (
            &super::engine::CAMOUFOX.version,
            super::engine::CAMOUFOX.folder(&installed),
        ),
        _ => (
            &super::engine::CFT.version,
            super::engine::CFT.folder(&installed),
        ),
    };
    assert!(
        engine_folder.join("manifest.json").is_file(),
        "{engine} {version} is not installed at {}: run `just cft` in latch/ \
         (or install.sh --state-dir into LATCH_CFT_STATE)",
        engine_folder.display()
    );
    let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.dev/tests");
    fs::create_dir_all(&parent).unwrap();
    let dir = TestDir {
        _turn: Turn::wait(places),
        dir: tempfile::tempdir_in(parent).unwrap(),
    };
    let state = dir.path().join("state");
    let area = dir.path().join("area");
    fs::create_dir_all(&state).unwrap();
    fs::create_dir_all(&area).unwrap();
    // This test's own state directory, sharing only the installed plugins.
    let plugins = installed.canonicalize().unwrap().join("plugins");
    std::os::unix::fs::symlink(&plugins, state.join("plugins")).unwrap();
    for name in profiles {
        profile::open(&state, name, engine).unwrap();
    }
    // The test sites are on 127.0.0.1 and localhost.
    fs::write(state.join("host.json"), r#"{"allow_local_browsing": true}"#).unwrap();
    (dir, state, area)
}

async fn the_engine_runs_from_the_host_with_its_own_sandbox_and_leaves_nothing(engine_name: &str) {
    let (dir, state, area) = test_state(engine_name, &["b1"]);

    let mut session =
        Session::start_with(&latch(), &state, "b1", Duration::from_secs(120), HEADLESS)
            .await
            .unwrap();
    println!("ready: {}", session.ready);
    let data = profile::root(&state)
        .join("b1/data")
        .canonicalize()
        .unwrap();
    let cft = engine_name != "camoufox";
    let (pipe, profile_arg) = if cft {
        (
            "--remote-debugging-pipe",
            format!("--user-data-dir={}", data.display()),
        )
    } else {
        ("-juggler-pipe", format!("-profile {}", data.display()))
    };
    let browser = host_pid(session.ready["pid"].as_u64().unwrap() as u32, &profile_arg);
    let scratch = session.scratch().canonicalize().unwrap();

    let url = local_page();
    let page = session
        .call(json!({"op": "open", "url": url}))
        .await
        .unwrap()
        .result;
    println!("open: {}", page["title"]);
    assert_eq!(page["title"], "latch local page");

    // Every engine process is in the browser's own process group, and
    // Camoufox's helper in one of its own.
    let all = processes();
    let mut engine: Vec<&Process> = all.iter().filter(|p| p.pgid == browser).collect();
    let main = *engine.iter().find(|p| p.pid == browser).unwrap();
    #[cfg(target_os = "macos")]
    {
        println!(
            "browser {}: sandboxed by Seatbelt: {}",
            main.pid,
            sandboxed(main.pid)
        );
        assert!(
            !sandboxed(main.pid),
            "the engine runs outside latch's Seatbelt"
        );
    }
    #[cfg(target_os = "linux")]
    let main_confinement = {
        let ours = confinement(std::process::id());
        let theirs = confinement(main.pid);
        println!(
            "browser {}: user namespace {}, seccomp {}; this test's {}",
            main.pid, theirs.0, theirs.1, ours.0
        );
        assert!(
            theirs.0 != ours.0 && theirs.1 == "2",
            "the engine runs inside latch's bubblewrap, with its filter on"
        );
        theirs
    };
    assert!(main.args.contains(pipe), "{}", main.args);
    assert!(main.args.contains(&profile_arg), "{}", main.args);
    // Its environment: only a plain PATH, and variables naming the scratch
    // folder or the engine's own settings.
    let names = environment(browser, &main.args);
    println!("engine environment: {names:?}");
    if cft {
        assert_eq!(names, ["BREAKPAD_DUMP_LOCATION", "HOME", "PATH", "TMPDIR"]);
    } else {
        let allowed = [
            "CAMOU_CONFIG_1",
            "HOME",
            "MOZ_CRASHREPORTER_DISABLE",
            "PATH",
            "TMPDIR",
        ];
        assert!(
            names.iter().all(|n| allowed.contains(&n.as_str())),
            "{names:?}"
        );
        assert!(names.iter().any(|n| n == "CAMOU_CONFIG_1") && names.iter().any(|n| n == "HOME"));
        // The helper, the engine's parent, gets the same few.
        let helper = all.iter().find(|p| p.pid == helper_of(browser)).unwrap();
        assert!(
            helper
                .args
                .contains(&scratch.join("helper.mjs").display().to_string())
        );
        engine.push(helper);
    }
    let mut helpers = 0;
    for p in engine.iter().filter(|p| p.pid != browser) {
        if cft {
            let kind = p
                .args
                .split(' ')
                .find_map(|arg| arg.strip_prefix("--type="))
                .unwrap_or("?");
            #[cfg(target_os = "macos")]
            {
                let client = p.args.contains("--seatbelt-client=");
                println!(
                    "helper {} --type={kind}: --seatbelt-client {client}, sandboxed by Seatbelt: {}",
                    p.pid,
                    sandboxed(p.pid)
                );
                if kind == "renderer" {
                    helpers += 1;
                    assert!(
                        client && sandboxed(p.pid),
                        "Chrome's sandbox holds the renderer"
                    );
                }
            }
            // Chrome's own namespace sandbox, nested in latch's.
            #[cfg(target_os = "linux")]
            {
                let (namespace, seccomp) = confinement(p.pid);
                println!(
                    "helper {} --type={kind}: user namespace {namespace}, seccomp {seccomp}",
                    p.pid
                );
                if kind == "renderer" {
                    helpers += 1;
                    assert!(
                        namespace != main_confinement.0 && seccomp == "2",
                        "Chrome's sandbox holds the renderer"
                    );
                }
            }
        } else {
            // Firefox's web content processes: `plugin-container … tab`.
            // Camoufox is pinned for macOS only.
            #[cfg(target_os = "macos")]
            {
                let content = p.args.contains("plugin-container") && p.args.ends_with(" tab");
                println!(
                    "process {}: web content {content}, sandboxed by Seatbelt: {}: {}",
                    p.pid,
                    sandboxed(p.pid),
                    p.args.split(" ").last().unwrap_or_default()
                );
                if content {
                    helpers += 1;
                    assert!(sandboxed(p.pid), "Firefox's sandbox holds web content");
                }
            }
        }
    }
    assert!(helpers > 0, "a web content process is running");
    let refused: &[&str] = if cft {
        &[
            "--no-sandbox",
            "--remote-debugging-port",
            "--single-process",
        ]
    } else {
        &[
            "-start-debugger-server",
            "-marionette",
            "--remote-debugging-port",
        ]
    };
    for p in &engine {
        for off in refused {
            assert!(!p.args.contains(off), "{} has {off}", p.pid);
        }
    }
    // Headless only when the host asks for it.
    let headless_arg = if cft { "--headless" } else { "-headless" };
    assert_eq!(main.args.contains(headless_arg), HEADLESS, "{}", main.args);

    // No engine process listens on TCP, seen from inside exec, and exec
    // can read neither the profile nor anything else of the state folder.
    let pids: Vec<u32> = engine.iter().map(|p| p.pid).collect();
    let (listeners, held) = tcp_listeners(&state, &area, &pids);
    println!("exec: {listeners} TCP listeners; engine pids among them: {held:?}");
    assert!(listeners > 0, "exec sees other processes' listeners");
    assert!(held.is_empty());
    // The engine's own settings file in the profile: Chrome's preferences,
    // or the fingerprint Camoufox keeps for the profile.
    let preferences = if cft {
        data.join("Default/Preferences")
    } else {
        data.parent().unwrap().join("camoufox.json")
    };
    for script in [
        format!("cat '{}'", preferences.display()),
        format!("ls '{}'", data.display()),
        format!(
            "ls '{}'",
            state.canonicalize().unwrap().join("browser").display()
        ),
    ] {
        let (ok, text) = exec(&state, &area, &script);
        println!("exec: {script}: {}", text.trim());
        assert!(!ok && text.contains(HIDDEN), "{script}: {text}");
    }

    let shot = session.call(json!({"op": "screenshot"})).await.unwrap();
    let (meta, png) = (shot.result, shot.bytes.unwrap());
    println!("screenshot: {meta}");
    assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert_eq!(meta["size"].as_u64().unwrap() as usize, png.len());
    assert!(meta["width"].as_u64().unwrap() > 0 && meta["height"].as_u64().unwrap() > 0);
    let shot = std::env::var_os("LATCH_BROWSER_EVIDENCE")
        .map(PathBuf::from)
        .unwrap_or_else(|| dir.path().to_path_buf())
        .join("local-page.png");
    fs::write(&shot, &png).unwrap();
    println!("screenshot written to {}", shot.display());

    let closed = session.close().await.unwrap();
    println!("close: {closed}");
    assert_eq!(closed["closed"], true);
    let left: Vec<String> = processes()
        .into_iter()
        .filter(|p| {
            p.pgid == browser
                || engine.iter().any(|e| e.pid == p.pid)
                || p.args.contains(&data.display().to_string())
                || p.args.contains(&scratch.display().to_string())
        })
        .map(|p| format!("{} {}", p.pid, p.args))
        .collect();
    println!("processes left: {}", left.len());
    assert!(left.is_empty(), "{left:#?}");
    assert!(!scratch.exists(), "the session's scratch folder is removed");

    // The profile stays, with password saving and autofill still off
    // (Camoufox ships with them off), and Camoufox's fingerprint private.
    let kept: serde_json::Value = serde_json::from_slice(&fs::read(&preferences).unwrap()).unwrap();
    if !cft {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&preferences).unwrap().permissions().mode();
        println!("profile kept: camoufox.json mode {:o}", mode & 0o777);
        assert_eq!(mode & 0o077, 0);
        assert!(kept["env"]["CAMOU_CONFIG_1"].is_string());
        return;
    }
    println!(
        "profile kept: credentials_enable_service {}, autofill.profile_enabled {}",
        kept["credentials_enable_service"], kept["autofill"]["profile_enabled"]
    );
    assert_eq!(kept["credentials_enable_service"], false);
    assert_eq!(kept["autofill"]["profile_enabled"], false);
}

/// Linux: no app is in front of an Xvfb display or a headless browser.
#[cfg(target_os = "linux")]
fn front_apps() -> std::sync::Arc<std::sync::Mutex<Vec<u32>>> {
    Default::default()
}

/// Watches the front app (by pid) every 40 ms, for as long as the test runs.
#[cfg(target_os = "macos")]
fn front_apps() -> std::sync::Arc<std::sync::Mutex<Vec<u32>>> {
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        loop {
            let pid = Command::new("/bin/sh")
                .args([
                    "-c",
                    "/usr/bin/lsappinfo info \"$(/usr/bin/lsappinfo front)\"",
                ])
                .output()
                .ok()
                .and_then(|out| {
                    let text = String::from_utf8_lossy(&out.stdout).into_owned();
                    let after = text.split("pid = ").nth(1)?;
                    after.split_whitespace().next()?.parse().ok()
                });
            if let Some(pid) = pid {
                let mut seen = log.lock().unwrap();
                if seen.last() != Some(&pid) {
                    seen.push(pid);
                }
            }
            std::thread::sleep(Duration::from_millis(40));
        }
    });
    seen
}

/// The ref of the first element with this role and name.
fn find(result: &serde_json::Value, role: &str, name: &str) -> String {
    result["elements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["role"] == role && e["name"] == name)
        .unwrap_or_else(|| panic!("no {role} {name:?} in {:#}", result["elements"]))["ref"]
        .as_str()
        .unwrap()
        .to_string()
}

fn element<'a>(result: &'a serde_json::Value, reference: &str) -> &'a serde_json::Value {
    result["elements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["ref"] == reference)
        .unwrap()
}

/// What the test page's `#out` line says, in a `text` result.
fn out_line(result: &serde_json::Value) -> String {
    result["text"]
        .as_str()
        .unwrap()
        .lines()
        .find(|line| {
            [
                "edited",
                "typed",
                "colour",
                "agree",
                "nothing yet",
                "alert",
                "waiting",
            ]
            .iter()
            .any(|start| line.starts_with(start))
        })
        .unwrap_or_default()
        .to_string()
}

async fn the_core_reads_frames_and_acts_by_ref_with_trusted_input(engine_name: &str) {
    let (_dir, state, _area) = test_state(engine_name, &["b2"]);
    let mut session =
        Session::start_with(&latch(), &state, "b2", Duration::from_secs(120), HEADLESS)
            .await
            .unwrap();
    // Every front app seen while the test runs: the engine must never be one.
    let engine = session.ready["pid"].as_u64().unwrap() as u32;
    let fronted = front_apps();
    let mut call = async |value: serde_json::Value| session.call(value).await.map(|r| r.result);
    let url = test_site();

    let page = call(json!({"op": "open", "url": url})).await.unwrap();
    println!("open: {} {}", page["title"], page["snapshot"]);
    assert_eq!(page["title"], "latch frames test");
    // Every frame, with its effective origin: same-origin, cross-site, and
    // a sandboxed one that is opaque.
    let frames = page["frames"].as_array().unwrap();
    println!("frames: {frames:#?}");
    let origins: Vec<&str> = frames
        .iter()
        .map(|f| f["origin"].as_str().unwrap())
        .collect();
    let pages_origin = origins[0].to_string();
    assert!(pages_origin.starts_with("http://127.0.0.1:"));
    assert_eq!(origins.iter().filter(|o| **o == pages_origin).count(), 2);
    assert!(origins.iter().any(|o| o.starts_with("http://localhost:")));
    assert!(origins.contains(&"opaque"));

    // Row text, values, and secrets listed without their value.
    let edit = find(&page, "button", "Edit");
    assert_eq!(element(&page, &edit)["row"], "Order 1001 Blue mug Edit");
    let password = find(&page, "textbox", "Password");
    assert_eq!(element(&page, &password)["secret"], true);
    assert!(element(&page, &password).get("value").is_none());
    assert!(!page.to_string().contains("made-up-secret"));
    let cross = find(&page, "button", "Cross-site button");
    assert!(
        element(&page, &cross)["frame"]
            .as_str()
            .unwrap()
            .starts_with('f')
    );

    let clicked = call(json!({"op": "act", "action": "click", "target": edit}))
        .await
        .unwrap();
    println!("click {edit}: {}", clicked["lines"]);
    assert_eq!(clicked["dispatched"], true);
    assert_eq!(
        out_line(&call(json!({"op": "text"})).await.unwrap()),
        "edited 1001, trusted true"
    );

    let name = find(&clicked, "textbox", "Name");
    let typed =
        call(json!({"op": "act", "action": "type", "target": name, "text": "Ada Lovelace"}))
            .await
            .unwrap();
    println!(
        "type {name}: {}",
        out_line(&call(json!({"op": "text"})).await.unwrap())
    );
    assert_eq!(
        out_line(&call(json!({"op": "text"})).await.unwrap()),
        "typed, trusted true"
    );
    assert_eq!(
        element(&typed, &find(&typed, "textbox", "Name"))["value"],
        "Ada Lovelace"
    );

    let password = find(&typed, "textbox", "Password");
    let refused = call(json!({"op": "act", "action": "type", "target": password, "text": "x"}))
        .await
        .unwrap_err();
    assert_eq!(
        (refused.exit, refused.code.as_str()),
        (crate::failure::Exit::Denied, "secret_field")
    );

    let colour = find(&typed, "combobox", "Colour");
    let selected = call(json!({"op": "act", "action": "select", "target": colour, "text": "Blue"}))
        .await
        .unwrap();
    println!(
        "select {colour}: {}",
        out_line(&call(json!({"op": "text"})).await.unwrap())
    );
    assert_eq!(
        out_line(&call(json!({"op": "text"})).await.unwrap()),
        "colour Blue, trusted true"
    );
    let missing = call(json!({"op": "act", "action": "select", "target": find(&selected, "combobox", "Colour"), "text": "Purple"}))
        .await
        .unwrap_err();
    assert_eq!(missing.code, "no_such_target");
    assert_eq!(missing.details["options"], json!(["Red", "Green", "Blue"]));

    let agree = find(&selected, "checkbox", "I agree");
    let checked = call(json!({"op": "act", "action": "click", "target": agree}))
        .await
        .unwrap();
    assert_eq!(
        out_line(&call(json!({"op": "text"})).await.unwrap()),
        "agree true, trusted true"
    );
    assert_eq!(
        element(&checked, &find(&checked, "checkbox", "I agree"))["checked"],
        true
    );

    // Into the cross-site and the sandboxed frame, by ref, with real clicks.
    for (button, pressed) in [
        ("Cross-site button", "cross pressed true"),
        ("Sandboxed button", "sandboxed pressed true"),
        ("Same-origin button", "same pressed true"),
    ] {
        let target = find(&checked, "button", button);
        let after = call(json!({"op": "act", "action": "click", "target": target}))
            .await
            .unwrap();
        println!(
            "click {target} ({button}): now {:?}",
            after["elements"]
                .as_array()
                .unwrap()
                .iter()
                .find(|e| e["name"] == pressed)
                .map(|e| &e["name"])
        );
        find(&after, "button", pressed);
    }

    // A ref from before a navigation is refused, never re-guessed.
    let observed = call(json!({"op": "observe"})).await.unwrap();
    let old_edit = find(&observed, "button", "Edit");
    let next = find(&observed, "link", "Next page");
    let moved = call(json!({"op": "act", "action": "click", "target": next}))
        .await
        .unwrap();
    assert_eq!(moved["changed"]["title"], "latch next page");
    let stale = call(json!({"op": "act", "action": "click", "target": old_edit, "snapshot": observed["snapshot"]}))
        .await
        .unwrap_err();
    println!("stale: {} {}", stale.code, stale.message);
    assert_eq!(stale.code, "stale_snapshot");
    assert!(
        stale.details["elements"].is_array(),
        "a fresh list comes with the refusal"
    );

    let back = call(json!({"op": "act", "action": "back"})).await.unwrap();
    assert_eq!(back["changed"]["title"], "latch frames test");

    // A tab latch opens is a window of its own, in the background: both
    // pages stay visible, and switching between them brings nothing forward.
    let visible = "document.visibilityState";
    let next_url = url.replace("frames.html", "next.html");
    let made = call(json!({"op": "tab", "action": "new", "url": next_url}))
        .await
        .unwrap();
    assert_eq!(made["title"], "latch next page");
    let made_tab = made["tab"].clone();
    assert_eq!(
        call(json!({"op": "eval", "script": visible}))
            .await
            .unwrap()["value"],
        "visible"
    );
    call(json!({"op": "tab", "action": "switch", "tab": "t1"}))
        .await
        .unwrap();
    assert_eq!(
        call(json!({"op": "eval", "script": visible}))
            .await
            .unwrap()["value"],
        "visible"
    );
    call(json!({"op": "tab", "action": "switch", "tab": made_tab}))
        .await
        .unwrap();
    let closed = call(json!({"op": "tab", "action": "close"})).await.unwrap();
    assert_eq!(closed["tab"], "t1");

    // Opening, reading, typing, clicking, navigating, going back and tabs
    // never bring the engine forward. (A page's own alert does: Chrome
    // raises the window it blocks, so this is checked before the alert.)
    assert!(
        !fronted.lock().unwrap().contains(&engine),
        "the engine became the front app"
    );

    // A dialog blocks the page until it is answered; a link opens a tab.
    let dialogs = call(
        json!({"op": "act", "action": "click", "target": find(&back, "link", "Dialogs and tabs")}),
    )
    .await
    .unwrap();
    let alerted = call(
        json!({"op": "act", "action": "click", "target": find(&dialogs, "button", "Show alert")}),
    )
    .await
    .unwrap();
    println!("alert: {}", alerted["events"]);
    assert_eq!(alerted["events"][0]["event"], "dialog");
    assert_eq!(alerted["events"][0]["message"], "Saved for order 1001");
    assert_eq!(alerted["blocked_by_dialog"], true);
    assert_eq!(
        call(json!({"op": "observe"})).await.unwrap_err().code,
        "dialog_open"
    );
    let answered = call(json!({"op": "dialog", "accept": true})).await.unwrap();
    assert_eq!(
        out_line(&call(json!({"op": "text"})).await.unwrap()),
        "alert closed"
    );

    let opened = call(json!({"op": "act", "action": "click", "target": find(&answered, "link", "Open next in a new tab")}))
        .await
        .unwrap();
    println!("new tab: {}", opened["events"]);
    let new_tab = opened["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["event"] == "new_tab")
        .unwrap()["tab"]
        .clone();
    let switched = call(json!({"op": "tab", "action": "switch", "tab": new_tab}))
        .await
        .unwrap();
    assert_eq!(switched["title"], "latch next page");
    let closed = call(json!({"op": "tab", "action": "close"})).await.unwrap();
    assert_eq!(closed["tabs"].as_array().unwrap().len(), 1);
    assert_eq!(closed["title"], "latch dialogs");

    let text = call(json!({"op": "text"})).await.unwrap();
    assert!(text["text"].as_str().unwrap().contains("alert closed"));

    drop(call);
    let closed = session.close().await.unwrap();
    assert_eq!(closed["closed"], true);
}

async fn frames_that_come_and_go_while_a_page_loads_break_no_call(engine: &str) {
    let (_dir, state, _area) = test_state_alone(engine, &["b2-churn"]);
    let mut session = Session::start_with(
        &latch(),
        &state,
        "b2-churn",
        Duration::from_secs(120),
        HEADLESS,
    )
    .await
    .unwrap();
    let url = test_site().replace("frames.html", "churn.html");
    let mut slowest = Duration::ZERO;
    for round in 0..3 {
        // Each load, and each reading after it, while cross-site frames
        // attach and go away underneath. The first call that fails ends
        // the test: the ones after it would only wait out their time.
        for n in 0..17 {
            let call = match n {
                0 => json!({"op": "open", "url": url}),
                _ if n % 2 == 1 => json!({"op": "observe"}),
                _ => json!({"op": "text"}),
            };
            let started = std::time::Instant::now();
            let done = session.call(call.clone()).await;
            slowest = slowest.max(started.elapsed());
            if let Err(failure) = done {
                panic!(
                    "round {round}, call {n} ({}) after {:?}: {} {}",
                    call["op"],
                    started.elapsed(),
                    failure.code,
                    failure.message
                );
            }
        }
    }
    println!("slowest call: {slowest:?}");
    let text = session.call(json!({"op": "text"})).await.unwrap().result;
    println!("{}", text["text"].as_str().unwrap_or_default().trim());
    session.close().await.unwrap();
}

async fn a_call_longer_than_the_idle_timeout_does_not_end_its_session(engine: &str) {
    let (_dir, state, _area) = test_state(engine, &["hang-idle"]);
    let mut session = Session::start_with(
        &latch(),
        &state,
        "hang-idle",
        Duration::from_secs(2),
        HEADLESS,
    )
    .await
    .unwrap();
    session
        .call(json!({"op": "open", "url": local_page()}))
        .await
        .unwrap();
    // One call that takes longer than the session may sit idle...
    let waited = session
        .call(json!({
            "op": "wait",
            "condition": {"kind": "text", "text": "never on this page"},
            "within_ms": 3000,
        }))
        .await
        .unwrap();
    assert_eq!(waited.result["met"], false);
    // ...and the next one, right after it, still finds the session.
    let text = session.call(json!({"op": "text"})).await.unwrap().result;
    assert!(
        text["text"].as_str().unwrap().contains("local page"),
        "{text}"
    );
    session.close().await.unwrap();
}

async fn a_browser_that_leaves_calls_unanswered_is_closed(engine: &str) {
    let (_dir, state, _area) = test_state(engine, &["hang-stuck"]);
    let scratch = tempfile::tempdir().unwrap();
    let mut command = Command::new(latch());
    command
        .arg("--state-dir")
        .arg(&state)
        .args(["browser-worker", "--profile", "hang-stuck", "--scratch"])
        .arg(scratch.path())
        .args(["--call-limit", "2"])
        // As the host's runtime passes it for a test state: the page is local.
        .arg("--allow-local-browsing");
    if HEADLESS {
        command.arg("--headless");
    }
    let mut worker = command
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .current_dir(scratch.path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = worker.stdin.take().unwrap();
    let mut output = std::io::BufReader::new(worker.stdout.take().unwrap());
    let mut answer = || {
        let mut line = String::new();
        std::io::BufRead::read_line(&mut output, &mut line).unwrap();
        serde_json::from_str::<serde_json::Value>(&line).unwrap_or_default()
    };
    let data = profile::root(&state).join("hang-stuck/data");
    let profile_arg = format!("--user-data-dir={}", data.canonicalize().unwrap().display());
    let browser =
        host_pid(answer()["ok"]["pid"].as_u64().unwrap() as u32, &profile_arg) as libc::pid_t;
    let mut call = |line: serde_json::Value| {
        writeln!(input, "{line}").unwrap();
        input.flush().unwrap();
    };
    call(json!({"op": "begin", "session": "k1"}));
    assert!(answer()["ok"].is_object());
    call(json!({"op": "open", "session": "k1", "url": local_page()}));
    assert!(answer()["ok"].is_object());

    // The browser stops answering anything.
    // SAFETY: kill sends a signal and touches no Rust memory.
    unsafe { libc::kill(browser, libc::SIGSTOP) };
    let mut codes = Vec::new();
    for _ in 0..3 {
        call(json!({"op": "observe", "session": "k1"}));
        codes.push(answer()["error"]["code"].clone());
    }
    // SAFETY: as above.
    unsafe { libc::kill(browser, libc::SIGCONT) };
    // Two calls time out; the third finds the browser gone, and the worker
    // closes it and ends.
    assert_eq!(codes, ["timeout", "timeout", "browser_gone"]);
    drop(input);
    let ended = worker.wait().unwrap();
    assert!(ended.success(), "{ended:?}");
}

/// The Camoufox helper gives up on a page's title after a wait of its own
/// and answers its operation without it; the title the browser never sent
/// is still a command left unanswered. Camoufox is pinned for macOS only.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn a_wait_the_camoufox_helper_gives_up_on_is_a_command_left_unanswered() {
    use super::camoufox::Camoufox;
    use super::driver::{Driver, Runtime};
    let (_dir, state, _area) = test_state("camoufox", &["helper-late"]);
    let scratch = tempfile::tempdir().unwrap();
    // The engine gets absolute paths, as the worker gives it.
    let state = state.canonicalize().unwrap();
    let scratch_path = scratch.path().canonicalize().unwrap();
    let proxy = super::egress::Proxy::start(true).unwrap().url();
    let mut browser =
        Camoufox::launch(&state, "helper-late", &scratch_path, HEADLESS, None, &proxy).unwrap();
    browser.begin("k1").unwrap();
    browser.navigate(&local_page()).unwrap();
    let pid = browser.pid() as libc::pid_t;
    // SAFETY: kill sends a signal and touches no Rust memory.
    unsafe { libc::kill(pid, libc::SIGSTOP) };
    // Longer than the helper waits for a title.
    browser.limit(Some(std::time::Instant::now() + Duration::from_secs(10)));
    let tabs = browser.tabs();
    let missed = browser.unanswered();
    // SAFETY: as above.
    unsafe { libc::kill(pid, libc::SIGCONT) };
    browser.limit(None);
    browser.close();
    assert_eq!(tabs.unwrap()[0].title, "");
    assert!(missed);
}

/// A dialog the page opens on its own, between two calls, holds the page
/// until it is answered: every call before then is refused with
/// dialog_open at once, never left waiting on the blocked page.
async fn a_dialog_that_opens_between_calls_is_refused_at_once(engine: &str) {
    let (_dir, state, _area) = test_state(engine, &["dialog-idle"]);
    let mut session = Session::start_with(
        &latch(),
        &state,
        "dialog-idle",
        Duration::from_secs(120),
        HEADLESS,
    )
    .await
    .unwrap();
    session
        .call(json!({"op": "open", "url": local_page()}))
        .await
        .unwrap();
    session
        .call(json!({"op": "eval", "script": "setTimeout(() => alert('later'), 2500); 1"}))
        .await
        .unwrap();
    // Long enough for the eval's own call to have ended first.
    tokio::time::sleep(Duration::from_millis(4500)).await;
    for op in ["observe", "observe", "observe", "text"] {
        let started = std::time::Instant::now();
        let refused = session.call(json!({"op": op})).await.err().expect(op);
        let took = started.elapsed();
        println!("{op}: {} after {took:?}", refused.code);
        assert_eq!(refused.code, "dialog_open", "{op}: {}", refused.message);
        assert!(took < Duration::from_secs(5), "{op} took {took:?}");
    }
    session
        .call(json!({"op": "dialog", "accept": true}))
        .await
        .unwrap();
    session.call(json!({"op": "observe"})).await.unwrap();
    session.close().await.unwrap();
}

/// A download from a frame latch has not read is traced to its session
/// through each tab's frames; a tab a dialog holds answers for none of
/// them, and another session's call does not wait on it.
async fn a_dialog_in_one_session_does_not_hold_another_sessions_download(engine: &str) {
    dialog_and_download(engine, 1000).await;
}

/// The same, with the download heard first and the dialog after it, both
/// taken in together by the next call.
async fn a_download_heard_before_another_sessions_dialog_does_not_wait_on_it(engine: &str) {
    dialog_and_download(engine, 2500).await;
}

/// Session a's page opens a dialog `alert_after` ms after its eval; session
/// b's page downloads from a frame 1.5 s after its own.
async fn dialog_and_download(engine: &str, alert_after: u64) {
    let (_dir, state, _area) = test_state(engine, &["dialog-download"]);
    let mut h = Host::new(&state);
    let (a, _) = h
        .open(CALLER, "dialog-download", json!({"url": local_page()}))
        .await;
    let (b, _) = h
        .open(CALLER, "dialog-download", json!({"url": local_page()}))
        .await;
    h.call(
        CALLER,
        "eval",
        &a,
        json!({"script": format!("setTimeout(() => alert('later'), {alert_after}); 1")}),
    )
    .await
    .unwrap();
    // A frame that downloads a file and is gone again before latch hears
    // of the download: no tab holds it, so every tab is asked.
    let script = "setTimeout(() => {\n\
        const frame = document.createElement('iframe');\n\
        frame.srcdoc = `<script>const a = document.createElement('a');\n\
          a.href = URL.createObjectURL(new Blob(['x'])); a.download = 'x.txt'; a.click();\n\
        <\\/script>`;\n\
        document.body.append(frame);\n\
        setTimeout(() => frame.remove(), 800);\n\
      }, 1500); 1";
    h.call(CALLER, "eval", &b, json!({"script": script}))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(4500)).await;
    let started = std::time::Instant::now();
    let listed = h.call(CALLER, "downloads", &b, json!({})).await;
    let took = started.elapsed();
    println!("b's downloads after {took:?}: {listed:?}");
    listed.unwrap();
    assert!(took < Duration::from_secs(5), "b's downloads took {took:?}");
    h.call(CALLER, "dialog", &a, json!({"accept": true}))
        .await
        .unwrap();
    for id in [&a, &b] {
        h.call(CALLER, "close", id, json!({})).await.unwrap();
    }
}

async fn a_frames_target_under_a_parent_page_layer_is_refused(engine: &str) {
    let (_dir, state, _area) = test_state(engine, &["b2-layer"]);
    let mut session = Session::start_with(
        &latch(),
        &state,
        "b2-layer",
        Duration::from_secs(120),
        HEADLESS,
    )
    .await
    .unwrap();
    let url = test_site().replace("frames.html", "overlay.html");
    let page = session
        .call(json!({"op": "open", "url": url}))
        .await
        .unwrap()
        .result;
    for button in ["Same-origin button", "Cross-site button"] {
        let target = find(&page, "button", button);
        let refused = session
            .call(json!({"op": "act", "action": "click", "target": target, "snapshot": page["snapshot"]}))
            .await
            .err()
            .expect("the click is refused");
        println!("{button}: {} {}", refused.code, refused.details["reason"]);
        assert_eq!(
            (refused.code.as_str(), &refused.details["reason"]),
            ("covered_target", &json!("covered"))
        );
    }
    let text = session.call(json!({"op": "text"})).await.unwrap().result;
    assert!(
        text["text"].as_str().unwrap().contains("layer clicks: 0"),
        "{}",
        text["text"]
    );
    session.close().await.unwrap();
}

async fn eval_upload_and_download_move_values_and_files(engine: &str) {
    let (_dir, state, _area) = test_state(engine, &["b3"]);
    let mut session =
        Session::start_with(&latch(), &state, "b3", Duration::from_secs(120), HEADLESS)
            .await
            .unwrap();
    let url = test_site().replace("frames.html", "files.html");
    let page = session
        .call(json!({"op": "open", "url": url}))
        .await
        .unwrap()
        .result;

    // The agent's script runs in the page's own world: it sees the page's
    // globals, may patch them, and its JSON value comes back.
    let script = "window.seen = []; const real = window.fetch;\n\
        window.fetch = (...a) => { seen.push(String(a[0])); return real(...a); };\n\
        fetch('report.csv').then(r => r.text()).then(t => ({title: document.title, fetched: seen, lines: t.trim().split('\\n').length}))";
    let evaluated = session
        .call(json!({"op": "eval", "script": script}))
        .await
        .unwrap()
        .result;
    println!("eval: {}", evaluated["value"]);
    assert_eq!(
        evaluated["value"],
        json!({"title": "latch files test", "fetched": ["report.csv"], "lines": 3})
    );
    let thrown = session
        .call(json!({"op": "eval", "script": "null.x"}))
        .await
        .err()
        .unwrap();
    assert_eq!(thrown.code, "eval_failed");

    // An upload: the page itself hashes the file it was given.
    let attach = find(&page, "button", "Attachment");
    let bytes = b"made-up notes for the upload test\n".repeat(100);
    let sha256 = crate::state::to_hex(&<sha2::Sha256 as sha2::Digest>::digest(&bytes));
    let uploaded = session
        .call_with(
            json!({"op": "upload", "target": attach, "name": "notes.txt", "size": bytes.len()}),
            Some(&bytes),
        )
        .await
        .unwrap()
        .result;
    assert_eq!(uploaded["dispatched"], true);
    let text = session.call(json!({"op": "text"})).await.unwrap().result;
    let line = format!("uploaded notes.txt {} bytes sha256 {sha256}", bytes.len());
    println!(
        "page says: {}",
        text["text"]
            .as_str()
            .unwrap()
            .lines()
            .find(|l| l.starts_with("uploaded"))
            .unwrap_or("")
    );
    assert!(
        text["text"].as_str().unwrap().contains(&line),
        "{}",
        text["text"]
    );

    // A download is a handle, then a file with the server's bytes.
    let link = find(&uploaded, "link", "Download the report");
    let started = std::time::Instant::now();
    let clicked = session
        .call(json!({"op": "act", "action": "click", "target": link}))
        .await
        .unwrap()
        .result;
    println!(
        "click events after {:?}: {}",
        started.elapsed(),
        clicked["events"]
    );
    // A download is no page load to wait out.
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(clicked["events"][0]["event"], "download");
    let got = session
        .call(json!({"op": "download_get", "download": "d1"}))
        .await
        .unwrap();
    let served =
        fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/browser-site/report.csv"))
            .unwrap();
    assert_eq!(got.result["name"], "report.csv");
    assert_eq!(got.bytes.unwrap(), served);
    let listed = session
        .call(json!({"op": "downloads"}))
        .await
        .unwrap()
        .result;
    println!("downloads: {}", listed["downloads"]);
    assert_eq!(listed["downloads"][0]["state"], "completed");
    let missing = session
        .call(json!({"op": "download_get", "download": "d9"}))
        .await
        .err()
        .unwrap();
    assert_eq!(missing.code, "no_such_download");
    session.close().await.unwrap();
}

/// Commands through the host's own path, each audit record kept.
struct Host {
    browsers: Browsers,
    audits: Vec<serde_json::Value>,
}

impl Host {
    fn new(state: &Path) -> Host {
        let mut browsers = Browsers::new(
            state.to_path_buf(),
            crate::vault::host::Vault::new(state.to_path_buf()),
            HEADLESS,
        );
        browsers.test = Some((latch(), Vec::new()));
        browsers.provider = Some(Arc::new(Calm));
        Host {
            browsers,
            audits: Vec::new(),
        }
    }

    /// `command` as `caller`, on session `name` (for `open`, its profile).
    async fn call(
        &mut self,
        caller: &str,
        command: &str,
        name: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, Failure> {
        let (result, audit) = run_as(&self.browsers, caller.into(), command, name, args).await;
        self.audits.push(audit);
        result
    }

    /// `open` as `caller` on `profile`: the new session's id, and the page.
    async fn open(
        &mut self,
        caller: &str,
        profile: &str,
        args: serde_json::Value,
    ) -> (String, serde_json::Value) {
        let page = self.call(caller, "open", profile, args).await.unwrap();
        (page["session"].as_str().unwrap().to_string(), page)
    }
}

/// One command as the host runs it, and its audit record.
async fn run_as(
    browsers: &Browsers,
    caller: host::Caller<'_>,
    command: &str,
    name: &str,
    args: serde_json::Value,
) -> (Result<serde_json::Value, Failure>, serde_json::Value) {
    let mut audit = serde_json::Map::new();
    let result = host::call(browsers, caller, command, name, &args, None, &mut audit)
        .await
        .map(|(result, _)| result);
    (result, serde_json::Value::Object(audit))
}

/// `act ACTION [REF] [TEXT]`, with a check after ` -> `, as the client
/// compiles it: a plan of one literal step.
fn act(line: &str) -> serde_json::Value {
    let (head, check) = match line.split_once(" -> ") {
        Some((head, check)) => (head, Some(super::plan::Check::parse(check).unwrap())),
        None => (line, None),
    };
    let mut words = head.splitn(3, ' ').map(str::to_string);
    let action =
        <crate::cli::ActAction as clap::ValueEnum>::from_str(&words.next().unwrap(), true).unwrap();
    let literal =
        super::client::act_literal(Some(action), None, words.next(), words.next()).unwrap();
    let step = super::plan::Step {
        literal: Some(literal),
        check,
        ..Default::default()
    };
    json!({
        "request": format!("r{:012x}", rand::random::<u64>() & 0xffff_ffff_ffff),
        "plan": super::plan::Plan { steps: vec![step], ..Default::default() },
        "act": true,
    })
}

/// A decision model that finds nothing risky: these tests are about
/// sessions, not confirmations.
struct Calm;

impl crate::model::Provider for Calm {
    fn ask<'a>(
        &'a self,
        _: &'a serde_json::Value,
        questions: &'a [crate::model::Question],
    ) -> crate::model::Asked<'a> {
        let answers = questions
            .iter()
            .map(|question| match question.id.as_str() {
                "risky" => (question.id.clone(), crate::model::Answer::Yes(0.02)),
                id => panic!("the calm model has no answer for {id}"),
            })
            .collect();
        let meta = crate::model::CallMeta {
            provider: "calm",
            model: "calm-1".into(),
            request_id: None,
            latency_ms: 0,
            cost: None,
        };
        Box::pin(async move { Ok((answers, meta)) })
    }
}

async fn sessions_on_one_profile_share_logins_and_nothing_else(engine: &str) {
    let (_dir, state, _area) = test_state(engine, &["b2b"]);
    let base = test_site()
        .split("/frames.html")
        .next()
        .unwrap()
        .to_string();
    let mut h = Host::new(&state);
    let other = "another-caller";
    let fronted = front_apps();

    // Two callers open the same profile: two sessions, one browser.
    let (a, _) = h
        .open(
            CALLER,
            "b2b",
            json!({"url": format!("{base}/alert.html"), "task": "task-a"}),
        )
        .await;
    let (b, _) = h
        .open(other, "b2b", json!({"url": format!("{base}/files.html")}))
        .await;
    println!(
        "ids: {} chars and {} chars, distinct: {}",
        a.len(),
        b.len(),
        a != b
    );
    assert_ne!(a, b);
    for id in [&a, &b] {
        assert_eq!(id.len(), 33, "s and 128 bits in hex");
    }
    let entry_a = h.browsers.session(&a, CALLER).await.unwrap();
    let entry_b = h.browsers.session(&b, other).await.unwrap();
    let engine = entry_a.runtime.ready["pid"].as_u64().unwrap() as u32;
    println!(
        "one runtime: {}, browser pid {} and {}",
        Arc::ptr_eq(&entry_a.runtime, &entry_b.runtime),
        engine,
        entry_b.runtime.ready["pid"]
    );
    assert!(Arc::ptr_eq(&entry_a.runtime, &entry_b.runtime));

    // A cookie one sets, the other reads: the profile's logins are shared.
    h.call(
        CALLER,
        "eval",
        &a,
        json!({"script": "document.cookie = 'signed_in=yes; path=/'; document.cookie"}),
    )
    .await
    .unwrap();
    let cookie = h
        .call(other, "eval", &b, json!({"script": "document.cookie"}))
        .await
        .unwrap();
    println!("cookie seen in the other session: {cookie}");
    assert!(cookie["value"].as_str().unwrap().contains("signed_in=yes"));

    // Two downloads at once with the same file name, each its own session's.
    let twin = |file: &str| format!("{base}/twin-download.html?file={file}");
    let mut links = Vec::new();
    for (caller, id, file) in [
        (CALLER, &a, "report.csv"),
        (other, &b, "invoice-2026-09.csv"),
    ] {
        h.call(
            caller,
            "tab",
            id,
            json!({"action": "new", "url": twin(file)}),
        )
        .await
        .unwrap();
        let page = h.call(caller, "observe", id, json!({})).await.unwrap();
        links.push(find(&page, "link", "Download the statement"));
    }
    let (got_a, got_b) = tokio::join!(
        run_as(
            &h.browsers,
            CALLER.into(),
            "do",
            &a,
            act(&format!("click {}", links[0]))
        ),
        run_as(
            &h.browsers,
            other.into(),
            "do",
            &b,
            act(&format!("click {}", links[1]))
        ),
    );
    got_a.0.unwrap();
    got_b.0.unwrap();
    let site = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/browser-site");
    for (caller, id, file) in [
        (CALLER, &a, "report.csv"),
        (other, &b, "invoice-2026-09.csv"),
    ] {
        let mut audit = serde_json::Map::new();
        let (got, bytes) = host::call(
            &h.browsers,
            caller.into(),
            "download_get",
            id,
            &json!({"download": "d1"}),
            None,
            &mut audit,
        )
        .await
        .unwrap();
        let listed = h.call(caller, "downloads", id, json!({})).await.unwrap();
        println!(
            "{caller}: {} download(s), d1 {} = {file}'s bytes: {}",
            listed["downloads"].as_array().unwrap().len(),
            got["name"],
            bytes.as_deref() == Some(&fs::read(site.join(file)).unwrap()[..])
        );
        assert_eq!(listed["downloads"].as_array().unwrap().len(), 1);
        assert_eq!(got["name"], "statement.csv");
        assert_eq!(bytes.unwrap(), fs::read(site.join(file)).unwrap());
    }

    // A page that asks for a passkey as it loads, with no conditional
    // mediation: the browser's or the OS's own sheet.
    h.call(
        CALLER,
        "tab",
        &a,
        json!({"action": "new", "url": format!("{base}/passkey-prompt.html")}),
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    h.call(CALLER, "tab", &a, json!({"action": "close"}))
        .await
        .unwrap();

    // Nothing so far brought the browser to the front: launch, a second
    // session's window, new tabs, downloads, a passkey prompt. (A page's
    // own popup or alert can: Chrome raises the window a page opens or
    // blocks, so this is checked first.)
    let seen_front = fronted.lock().unwrap().clone();
    println!("front apps seen: {seen_front:?}; engine {engine}");
    assert!(!seen_front.contains(&engine));

    // A popup belongs to the session whose page opened it.
    h.call(CALLER, "tab", &a, json!({"action": "switch", "tab": "t1"}))
        .await
        .unwrap();
    let page_a = h.call(CALLER, "observe", &a, json!({})).await.unwrap();
    let link = find(&page_a, "link", "Open next in a new tab");
    let clicked = h
        .call(CALLER, "do", &a, act(&format!("click {link}")))
        .await
        .unwrap();
    println!("popup in a: {}", clicked["trail"][0]["events"]);
    let tabs = |seen: &serde_json::Value| {
        seen["tabs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tab| {
                tab["url"]
                    .as_str()
                    .unwrap()
                    .rsplit('/')
                    .next()
                    .unwrap()
                    .to_string()
            })
            .collect::<Vec<_>>()
    };
    let seen_a = h.call(CALLER, "observe", &a, json!({})).await.unwrap();
    let seen_b = h.call(other, "observe", &b, json!({})).await.unwrap();
    println!("tabs: a {:?}, b {:?}", tabs(&seen_a), tabs(&seen_b));
    assert_eq!(
        tabs(&seen_a),
        [
            "alert.html",
            "twin-download.html?file=report.csv",
            "next.html"
        ]
    );
    assert_eq!(seen_a["tabs"][2]["opener"], "t1");
    assert_eq!(
        tabs(&seen_b),
        ["files.html", "twin-download.html?file=invoice-2026-09.csv"]
    );

    // The plan went on in the popup; back to the first tab.
    assert_eq!(seen_a["tab"], clicked["trail"][0]["followed"]);
    h.call(CALLER, "tab", &a, json!({"action": "switch", "tab": "t1"}))
        .await
        .unwrap();

    // A dialog is its own session's: the other reads on and has none.
    let page_a = h.call(CALLER, "observe", &a, json!({})).await.unwrap();
    let shown = find(&page_a, "button", "Show alert");
    let clicked = h
        .call(CALLER, "do", &a, act(&format!("click {shown}")))
        .await
        .unwrap();
    println!("dialog in a: {}", clicked["trail"][0]["events"]);
    assert_eq!(clicked["trail"][0]["events"][0]["event"], "dialog");
    let seen = h.call(other, "observe", &b, json!({})).await.unwrap();
    assert_eq!(seen["title"], "latch twin downloads");
    let none = h
        .call(other, "dialog", &b, json!({"accept": true}))
        .await
        .unwrap_err();
    println!("dialog accept in b: {}", none.code);
    assert_eq!(none.code, "no_dialog");
    h.call(CALLER, "dialog", &a, json!({"accept": true}))
        .await
        .unwrap();

    // The id is the capability: another caller with the right id gets
    // not_owner for every command, and nothing reaches the session.
    let mut refused = Vec::new();
    for command in [
        "observe",
        "text",
        "screenshot",
        "tab",
        "dialog",
        "eval",
        "downloads",
        "download_get",
        "do",
        "find",
        "wait",
        "login",
        "close",
    ] {
        let failure = h.call(other, command, &a, json!({})).await.unwrap_err();
        refused.push((command, failure.exit, failure.code));
    }
    println!("b on a's id: {refused:?}");
    assert!(
        refused
            .iter()
            .all(|(_, exit, code)| *exit == crate::failure::Exit::Denied && code == "not_owner")
    );

    // Two tasks under one client key: each open is a session of its own,
    // and without a session's id, the other can't reach it.
    let (c, _) = h
        .open(
            CALLER,
            "b2b",
            json!({"url": format!("{base}/next.html"), "task": "task-c"}),
        )
        .await;
    assert_ne!(a, c);
    let guessed = format!("s{}", "0".repeat(32));
    let hash_a = h.audits[0]["session"].as_str().unwrap().to_string();
    for (command, args) in [
        ("observe", json!({})),
        ("do", act("scroll down")),
        ("close", json!({})),
    ] {
        for id in [&guessed, &hash_a] {
            let failure = h.call(CALLER, command, id, args.clone()).await.unwrap_err();
            assert_eq!(failure.code, "no_such_session", "{command}");
        }
    }
    let listed = h.call(CALLER, "sessions", "", json!({})).await.unwrap();
    println!("sessions: {listed}");
    assert_eq!(listed["sessions"].as_array().unwrap().len(), 2);
    let text = listed.to_string();
    assert!(!text.contains(&a) && !text.contains(&c) && !text.contains(&b));
    let theirs = h.call(other, "sessions", "", json!({})).await.unwrap();
    assert_eq!(theirs["sessions"].as_array().unwrap().len(), 1);

    // Closing one session leaves the others' windows working.
    h.call(CALLER, "close", &a, json!({})).await.unwrap();
    let seen = h.call(other, "observe", &b, json!({})).await.unwrap();
    assert_eq!(seen["title"], "latch twin downloads");
    let seen = h.call(CALLER, "observe", &c, json!({})).await.unwrap();
    assert_eq!(seen["title"], "latch next page");
    assert!(entry_b.runtime.alive());
    assert_eq!(
        h.call(CALLER, "observe", &a, json!({}))
            .await
            .unwrap_err()
            .code,
        "no_such_session"
    );

    // The audit: a short hash of each id, never an id; the task label.
    let records = serde_json::Value::Array(h.audits.clone()).to_string();
    for id in [&a, &b, &c] {
        assert!(!records.contains(id.as_str()));
    }
    assert!(
        h.audits
            .iter()
            .filter_map(|r| r["session"].as_str())
            .all(|hash| hash.len() == 12)
    );
    assert_eq!(h.audits[0]["task"], "task-a");
    println!(
        "audit: {} records, ids never; first {}",
        h.audits.len(),
        h.audits[0]
    );

    h.call(CALLER, "close", &c, json!({})).await.unwrap();

    // An owner device ends any session by its hash, as the dashboard shows
    // it; an agent can't, and nobody closes another's session by its id.
    let hash_b = h.audits[1]["session"].as_str().unwrap().to_string();
    let device = host::Caller {
        key: "owner-device",
        device: true,
        gate: None,
    };
    let agent = h
        .call(CALLER, "close", &hash_b, json!({}))
        .await
        .unwrap_err();
    let (by_id, _) = run_as(&h.browsers, device, "close", &b, json!({})).await;
    println!(
        "close b: an agent by hash {}, the device by id {}",
        agent.code,
        by_id.as_ref().unwrap_err().code
    );
    assert_eq!(agent.code, "no_such_session");
    assert_eq!(by_id.unwrap_err().code, "not_owner");
    let (closed, record) = run_as(&h.browsers, device, "close", &hash_b, json!({})).await;
    println!(
        "the device by hash: {}; audit {record}",
        closed.as_ref().unwrap()
    );
    assert_eq!(closed.unwrap()["closed"], true);
    assert_eq!(record["session"], json!(hash_b));
    assert_eq!(record["by_owner_device"], true);
    let gone = h.call(other, "observe", &b, json!({})).await.unwrap_err();
    assert_eq!(gone.code, "no_such_session");
    println!(
        "runtime after the last close: alive {}",
        entry_b.runtime.alive()
    );
    assert!(!entry_b.runtime.alive());
}

async fn sessions_warn_on_a_shared_site_and_a_reserved_one_is_refused(engine: &str) {
    let (_dir, state, _area) = test_state(engine, &["b2b-site"]);
    let frames = test_site();
    let base = frames.split("/frames.html").next().unwrap().to_string();
    let cross = frames.split("cross=").nth(1).unwrap();
    let elsewhere = format!("http://localhost:{cross}");
    let mut h = Host::new(&state);
    let other = "another-caller";

    // Both on 127.0.0.1: the second answer there names the first session.
    let (a, _) = h
        .open(
            CALLER,
            "b2b-site",
            json!({"url": format!("{base}/next.html")}),
        )
        .await;
    let hash_a = h.audits[0]["session"].clone();
    let (b, page_b) = h
        .open(
            other,
            "b2b-site",
            json!({"url": format!("{base}/alert.html")}),
        )
        .await;
    println!("b's open: shared_site {}", page_b["shared_site"]);
    assert_eq!(page_b["shared_site"][0]["session"], hash_a);
    assert_eq!(h.audits[1]["shared_site"], json!([hash_a]));
    h.call(CALLER, "close", &a, json!({})).await.unwrap();

    // A reservation: another session's action on the site is refused
    // before anything is sent; reading goes on; elsewhere goes on.
    let (r, _) = h
        .open(
            CALLER,
            "b2b-site",
            json!({"url": format!("{base}/next.html"), "reserve_site": true}),
        )
        .await;
    let page = h.call(other, "observe", &b, json!({})).await.unwrap();
    let shown = find(&page, "button", "Show alert");
    let busy = h
        .call(other, "do", &b, act(&format!("click {shown}")))
        .await
        .unwrap_err();
    println!(
        "b clicks on the reserved site: {} ({})",
        busy.code, busy.message
    );
    assert_eq!(
        (busy.exit, busy.code.as_str()),
        (crate::failure::Exit::Failed, "site_busy")
    );
    let text = h.call(other, "text", &b, json!({})).await.unwrap();
    assert!(
        text["text"].as_str().unwrap().contains("waiting"),
        "nothing was clicked"
    );
    for (command, args) in [
        (
            "tab",
            json!({"action": "new", "url": format!("{base}/next.html")}),
        ),
        ("eval", json!({"script": "1"})),
    ] {
        let failure = h.call(other, command, &b, args).await.unwrap_err();
        assert_eq!(failure.code, "site_busy", "{command}");
    }
    let opened = h
        .call(
            other,
            "open",
            "b2b-site",
            json!({"url": format!("{base}/files.html")}),
        )
        .await
        .unwrap_err();
    println!("a new session's open on it: {}", opened.code);
    assert_eq!(opened.code, "site_busy");
    let away = h
        .call(
            other,
            "tab",
            &b,
            json!({"action": "new", "url": format!("{elsewhere}/next.html")}),
        )
        .await
        .unwrap();
    println!("b elsewhere: {}", away["tab"]);
    // The reservation ends with its session.
    h.call(CALLER, "close", &r, json!({})).await.unwrap();
    h.call(other, "tab", &b, json!({"action": "switch", "tab": "t1"}))
        .await
        .unwrap();
    let clicked = h
        .call(other, "do", &b, act(&format!("click {shown}")))
        .await
        .unwrap();
    println!("after close: {}", clicked["trail"][0]["events"]);
    h.call(other, "dialog", &b, json!({"accept": true}))
        .await
        .unwrap();
    h.call(other, "close", &b, json!({})).await.unwrap();
}

async fn navigating_into_a_reserved_site_is_refused_before_it_is_sent(engine: &str) {
    let (_dir, state, _area) = test_state(engine, &["b2b-leave"]);
    let frames = test_site();
    let local = frames.split("/frames.html").next().unwrap().to_string();
    let cross = frames.split("cross=").nth(1).unwrap();
    let elsewhere = format!("http://localhost:{cross}");
    let mut h = Host::new(&state);
    let other = "another-caller";

    // The other session: a page on 127.0.0.1 it left for localhost, then a
    // tab on localhost whose link and form lead back to 127.0.0.1.
    let leave = |from: &str, to: &str| format!("{from}/leave.html?to={to}/next.html");
    let (b, page) = h
        .open(
            other,
            "b2b-leave",
            json!({"url": leave(&local, &elsewhere)}),
        )
        .await;
    let go = find(&page, "link", "Go there");
    h.call(other, "do", &b, act(&format!("click {go}")))
        .await
        .unwrap();
    h.call(
        other,
        "tab",
        &b,
        json!({"action": "new", "url": leave(&elsewhere, &local)}),
    )
    .await
    .unwrap();
    let page = h.call(other, "observe", &b, json!({})).await.unwrap();

    // 127.0.0.1 reserved: no way into it is sent.
    let (r, _) = h
        .open(
            CALLER,
            "b2b-leave",
            json!({"url": format!("{local}/next.html"), "reserve_site": true}),
        )
        .await;
    let search = find(&page, "textbox", "Search");
    // The second form's own action stays on localhost: only its submit
    // button's formaction leads to 127.0.0.1, and Enter in its field goes
    // there too.
    let field = find(&page, "textbox", "Find");
    let mut outcomes = Vec::new();
    for line in [
        format!("click {}", find(&page, "link", "Go there")),
        format!("click {}", find(&page, "button", "Send")),
        format!("press {search} Enter"),
        format!("press {field} Enter"),
        format!("type {field} x\n"),
    ] {
        let outcome = h.call(other, "do", &b, act(&line)).await;
        outcomes.push((line, outcome.map_or_else(|f| f.code, |_| "sent".into())));
    }
    let went = h
        .call(
            other,
            "goto",
            &b,
            json!({"url": format!("{local}/next.html")}),
        )
        .await;
    outcomes.push((
        "goto".into(),
        went.map_or_else(|f| f.code, |_| "sent".into()),
    ));
    let tab = h.call(other, "observe", &b, json!({})).await.unwrap();
    println!(
        "into the reserved site from localhost: {outcomes:?}; now on {}",
        tab["url"]
    );
    h.call(other, "tab", &b, json!({"action": "switch", "tab": "t1"}))
        .await
        .unwrap();
    let back = h.call(other, "do", &b, act("back")).await;
    let back = back.map_or_else(|f| f.code, |_| "sent".into());
    let stayed = h.call(other, "observe", &b, json!({})).await.unwrap();
    println!("back into it: {back}; now on {}", stayed["url"]);
    assert!(outcomes.iter().all(|(_, code)| code == "site_busy"));
    assert!(tab["url"].as_str().unwrap().starts_with(&elsewhere));
    assert_eq!(back, "site_busy");
    assert_eq!(stayed["url"], json!(format!("{elsewhere}/next.html")));
    // Within localhost, the other session goes on.
    let home = find(&stayed, "link", "Back to orders");
    h.call(other, "do", &b, act(&format!("click {home}")))
        .await
        .unwrap();

    // Once the reservation ends, so does the refusal.
    h.call(CALLER, "close", &r, json!({})).await.unwrap();
    h.call(other, "do", &b, act("back")).await.unwrap();
    let returned = h.call(other, "do", &b, act("back")).await.unwrap();
    println!("after close, back: {}", returned["page"]["url"]);
    assert!(
        returned["page"]["url"]
            .as_str()
            .unwrap()
            .starts_with(&local)
    );
    h.call(other, "close", &b, json!({})).await.unwrap();
}

/// Back is checked against the tab's real history: after A, B and A
/// again, Back leads to B, not to whatever came before the first A.
async fn back_into_a_reserved_site_is_refused_after_a_return_visit(engine: &str) {
    let (_dir, state, _area) = test_state(engine, &["b8-back"]);
    let frames = test_site();
    let a = frames.replace("frames.html", "next.html");
    let cross = frames.split("cross=").nth(1).unwrap();
    let b = format!("http://localhost:{cross}/next.html");
    let mut session = Session::start_with(
        &latch(),
        &state,
        "b8-back",
        Duration::from_secs(120),
        HEADLESS,
    )
    .await
    .unwrap();
    for url in [&a, &b, &a] {
        session
            .call(json!({"op": "open", "url": url}))
            .await
            .unwrap();
    }
    // Another session on the profile reserves B's site.
    let holder = Session::begin(&session.runtime).await.unwrap();
    let site = crate::vault::sites::site_of(&b).unwrap();
    assert!(session.runtime.reserve(&site, holder.key()));
    let back = session.call(json!({"op": "act", "action": "back"})).await;
    let back = back.map_or_else(|f| f.code, |_| "sent".into());
    let now = session.call(json!({"op": "observe"})).await.unwrap().result;
    println!("back from the return visit: {back}; now on {}", now["url"]);
    assert_eq!(back, "site_busy");
    assert_eq!(now["url"], json!(a));
    // A page of A's own site first: Back leads back to A, and goes.
    let a2 = a.replace("next.html", "alert.html");
    session
        .call(json!({"op": "open", "url": a2}))
        .await
        .unwrap();
    let back = session.call(json!({"op": "act", "action": "back"})).await;
    let back = back.map_or_else(|f| f.code, |_| "sent".into());
    let now = session.call(json!({"op": "observe"})).await.unwrap().result;
    println!("back within A's site: {back}; now on {}", now["url"]);
    assert_eq!((back.as_str(), &now["url"]), ("sent", &json!(a)));
    session.close().await.unwrap();
}

/// Camoufox runs with the prefs that open every window a page opens
/// (`window.open`, with features or without, or a `target=_blank` link) as a
/// tab that loads in the background; Firefox keeps the ones that differ from
/// its defaults in the profile's `prefs.js` as it quits. Whether a window
/// then stays behind the owner's needs a screen to tell: Playwright keeps
/// every page active, and Camoufox reports a made-up screen. Camoufox is
/// pinned for macOS only.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn camoufox_runs_with_page_windows_as_background_tabs() {
    let (_dir, state, _area) = test_state("camoufox", &["fox-tabs"]);
    let session = Session::start_with(
        &latch(),
        &state,
        "fox-tabs",
        Duration::from_secs(120),
        HEADLESS,
    )
    .await
    .unwrap();
    session.close().await.unwrap();
    let prefs = fs::read_to_string(profile::root(&state).join("fox-tabs/data/prefs.js")).unwrap();
    if HEADLESS {
        assert!(
            prefs.contains(r#"user_pref("widget.macos.allow-native-select", false);"#),
            "headless Firefox must disable native macOS select menus"
        );
    }
    for pref in [
        r#"user_pref("browser.link.open_newwindow.restriction", 0);"#,
        r#"user_pref("browser.tabs.loadDivertedInBackground", true);"#,
    ] {
        assert!(prefs.contains(pref), "{pref} is not in force");
    }
}

async fn one_sessions_wait_does_not_hold_the_browser_for_another(engine: &str) {
    let (_dir, state, _area) = test_state(engine, &["b2b-turns"]);
    let frames = test_site();
    let base = frames.split("/frames.html").next().unwrap().to_string();
    let cross = frames.split("cross=").nth(1).unwrap();
    let mut h = Host::new(&state);
    let (a, _) = h
        .open(
            CALLER,
            "b2b-turns",
            json!({"url": format!("{base}/next.html")}),
        )
        .await;
    let (b, _) = h
        .open(
            CALLER,
            "b2b-turns",
            json!({"url": format!("http://localhost:{cross}/files.html")}),
        )
        .await;
    let started = std::time::Instant::now();
    let slow = async {
        let (result, _) = run_as(
            &h.browsers,
            CALLER.into(),
            "do",
            &a,
            act("scroll down -> text: never on this page"),
        )
        .await;
        (result, started.elapsed())
    };
    let quick = async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let mut done = Vec::new();
        for line in ["scroll down", "scroll up", "scroll down"] {
            let (result, _) = run_as(&h.browsers, CALLER.into(), "do", &b, act(line)).await;
            result.unwrap();
            done.push(started.elapsed());
        }
        done
    };
    let ((slow, slow_took), quick) = tokio::join!(slow, quick);
    println!(
        "a's step stopped {} after {slow_took:?}; b's three steps done at {quick:?}",
        slow.unwrap_err().code
    );
    // A's check waits out its 10 s; b's steps don't wait for it.
    assert!(quick.last().unwrap() < &Duration::from_secs(3));
    assert!(slow_took >= Duration::from_secs(9));
    for id in [&a, &b] {
        h.call(CALLER, "close", id, json!({})).await.unwrap();
    }
}

/// An answer shows the elements on screen and the frames they are in;
/// `observe --all` lists the rest, whose refs work, and a scroll brings
/// them into the bounded view.
async fn an_answer_shows_the_screen_and_observe_all_every_element(engine: &str) {
    let (_dir, state, _area) = test_state(engine, &["shown"]);
    let page = br#"<!doctype html><title>long page</title>
        <button>Top</button>
        <iframe src="/frame.html" style="height:80px"></iframe>
        <div style="height:4000px"></div>
        <button onclick="this.after('Bottom pressed')">Bottom</button>"#;
    let frame = b"<!doctype html><button>In frame</button>";
    let port = serve(vec![
        ("/".into(), page.to_vec()),
        ("/frame.html".into(), frame.to_vec()),
    ]);
    let mut h = Host::new(&state);
    let (id, opened) = h
        .open(
            CALLER,
            "shown",
            json!({"url": format!("http://127.0.0.1:{port}/")}),
        )
        .await;
    let names = |page: &serde_json::Value| -> Vec<String> {
        page["elements"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["name"].as_str().unwrap().to_string())
            .collect()
    };
    println!("open: {:?}; omitted: {}", names(&opened), opened["omitted"]);
    assert_eq!(names(&opened), ["Top", "In frame"]);
    assert_eq!(opened["frames"][1]["frame"], "f1");
    assert!(
        opened["omitted"]
            .as_str()
            .unwrap()
            .starts_with("1 element (off screen")
    );

    let all = h
        .call(CALLER, "observe", &id, json!({"all": true}))
        .await
        .unwrap();
    assert_eq!(names(&all), ["Top", "Bottom", "In frame"]);
    assert!(all.get("omitted").is_none());
    let bottom = find(&all, "button", "Bottom");
    let clicked = h
        .call(
            CALLER,
            "do",
            &id,
            act(&format!("click {bottom} -> text: Bottom pressed")),
        )
        .await
        .unwrap();
    assert_eq!(clicked["status"], "reached");
    // Scrolled to it by the click, it is on screen now.
    let now = h.call(CALLER, "observe", &id, json!({})).await.unwrap();
    println!("after the click: {:?}", names(&now));
    assert!(names(&now).contains(&"Bottom".to_string()));
    assert!(!names(&now).contains(&"Top".to_string()));

    // A page's own value, however it looks, is not latch's page.
    let mine = json!({"snapshot": "o9", "elements": [{"ref": "e1", "offscreen": true}]});
    let evaluated = h
        .call(CALLER, "eval", &id, json!({"script": format!("({mine})")}))
        .await
        .unwrap();
    assert_eq!(evaluated["value"], mine);
    h.call(CALLER, "close", &id, json!({})).await.unwrap();
}

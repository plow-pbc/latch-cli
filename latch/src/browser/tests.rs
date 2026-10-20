//! A real session on the host: the pinned Chrome for Testing, headed, from
//! the host's browser worker, run from a logged-in desktop session. It
//! needs the engine installed where `just cft` (part of `just test`, and a
//! step in CI) puts it, `.dev/cft-state`, or in the state directory
//! `LATCH_CFT_STATE` names; without it the test fails, saying so.
//!
//! `LATCH_BROWSER_EVIDENCE=DIR` also keeps the screenshot there.

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

mod fills;
mod steps;
use super::session::Session;

/// The caller every host-level test calls as.
const CALLER: &str = "test-caller";

unsafe extern "C" {
    /// macOS: 1 if the process is inside a Seatbelt sandbox (with no
    /// operation named), 0 if not.
    fn sandbox_check(pid: libc::pid_t, operation: *const c_char, kind: c_int, ...) -> c_int;
}

fn sandboxed(pid: u32) -> bool {
    // SAFETY: no operation or filter arguments; reads the pid's sandbox state.
    unsafe { sandbox_check(pid as libc::pid_t, std::ptr::null(), 0) == 1 }
}

struct Process {
    pid: u32,
    pgid: u32,
    args: String,
}

fn processes() -> Vec<Process> {
    let out = Command::new("/bin/ps")
        .args(["-axww", "-o", "pid=,pgid=,args="])
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

/// Serves `pages` (path, HTML) on 127.0.0.1 for as long as the test runs;
/// returns the port.
fn serve(pages: Vec<(String, Vec<u8>)>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for socket in listener.incoming() {
            let Ok(mut socket) = socket else { continue };
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

/// A fresh state directory for one test, sharing only the installed
/// plugins, and a folder to share with `exec`. The engine must be
/// installed (see the top of this file).
fn test_state() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let installed = std::env::var_os("LATCH_CFT_STATE")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../.dev/cft-state"));
    let engine_folder = super::engine::CFT.folder(&installed);
    assert!(
        engine_folder.join("manifest.json").is_file(),
        "Chrome for Testing {} is not installed at {}: run `just cft` in latch/ \
         (or install.sh --state-dir into LATCH_CFT_STATE)",
        super::engine::CFT.version,
        engine_folder.display()
    );
    let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.dev/tests");
    fs::create_dir_all(&parent).unwrap();
    let dir = tempfile::tempdir_in(parent).unwrap();
    let state = dir.path().join("state");
    let area = dir.path().join("area");
    fs::create_dir_all(&state).unwrap();
    fs::create_dir_all(&area).unwrap();
    // This test's own state directory, sharing only the installed plugins.
    let plugins = installed.canonicalize().unwrap().join("plugins");
    std::os::unix::fs::symlink(&plugins, state.join("plugins")).unwrap();
    (dir, state, area)
}

#[tokio::test]
async fn cft_runs_headed_from_the_host_with_its_own_sandbox_and_leaves_nothing() {
    let (dir, state, area) = test_state();

    let mut session = Session::start_with(&latch(), &state, "b1", Duration::from_secs(120))
        .await
        .unwrap();
    println!("ready: {}", session.ready);
    let browser = session.ready["pid"].as_u64().unwrap() as u32;
    let data = profile::root(&state)
        .join("b1/data")
        .canonicalize()
        .unwrap();
    let scratch = session.scratch().canonicalize().unwrap();

    let url = local_page();
    let page = session
        .call(json!({"op": "open", "url": url}))
        .await
        .unwrap()
        .result;
    println!("open: {}", page["title"]);
    assert_eq!(page["title"], "latch local page");

    // Every engine process is in the browser's own process group.
    let engine: Vec<Process> = processes()
        .into_iter()
        .filter(|p| p.pgid == browser)
        .collect();
    let main = engine.iter().find(|p| p.pid == browser).unwrap();
    println!(
        "browser {}: sandboxed by Seatbelt: {}",
        main.pid,
        sandboxed(main.pid)
    );
    assert!(
        !sandboxed(main.pid),
        "the engine runs outside latch's Seatbelt"
    );
    assert!(main.args.contains("--remote-debugging-pipe"));
    assert!(
        main.args
            .contains(&format!("--user-data-dir={}", data.display()))
    );
    // Its environment: four variables, all naming the scratch folder or a
    // plain PATH (ps -E prints them after the arguments).
    let with_env = Command::new("/bin/ps")
        .args(["-wwE", "-o", "command=", "-p", &browser.to_string()])
        .output()
        .unwrap();
    let with_env = String::from_utf8_lossy(&with_env.stdout);
    // The engine's arguments end with its profile folder.
    let last_arg = format!("--user-data-dir={} ", data.display());
    let env = with_env.trim().split_once(&last_arg).unwrap().1;
    let names: Vec<&str> = env
        .split(' ')
        .filter_map(|v| v.split_once('='))
        .map(|v| v.0)
        .collect();
    println!(
        "engine environment: {}",
        env.replace(&scratch.display().to_string(), "<scratch>")
    );
    assert_eq!(names, ["BREAKPAD_DUMP_LOCATION", "HOME", "PATH", "TMPDIR"]);
    let mut helpers = 0;
    for p in engine.iter().filter(|p| p.pid != browser) {
        let kind = p
            .args
            .split(' ')
            .find_map(|arg| arg.strip_prefix("--type="))
            .unwrap_or("?");
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
    assert!(helpers > 0, "a renderer is running");
    for p in &engine {
        for off in [
            "--no-sandbox",
            "--remote-debugging-port",
            "--headless",
            "--single-process",
        ] {
            assert!(!p.args.contains(off), "{} has {off}", p.pid);
        }
    }

    // No engine process listens on TCP, seen from inside exec, and exec
    // can read neither the profile nor anything else of the state folder.
    let (_, listening) = exec(&state, &area, "lsof -nP -iTCP -sTCP:LISTEN -F p");
    let listeners: Vec<u32> = listening
        .lines()
        .filter_map(|line| line.strip_prefix('p')?.parse().ok())
        .collect();
    println!(
        "exec: {} processes listen on TCP; engine pids among them: {:?}",
        listeners.len(),
        engine
            .iter()
            .filter(|p| listeners.contains(&p.pid))
            .map(|p| p.pid)
            .collect::<Vec<_>>()
    );
    assert!(
        !listeners.is_empty(),
        "exec's lsof sees other processes' listeners"
    );
    assert!(engine.iter().all(|p| !listeners.contains(&p.pid)));
    let preferences = data.join("Default/Preferences");
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
        assert!(
            !ok && text.contains("Operation not permitted"),
            "{script}: {text}"
        );
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

    // The profile stays, with password saving and autofill still off.
    let kept: serde_json::Value = serde_json::from_slice(&fs::read(&preferences).unwrap()).unwrap();
    println!(
        "profile kept: credentials_enable_service {}, autofill.profile_enabled {}",
        kept["credentials_enable_service"], kept["autofill"]["profile_enabled"]
    );
    assert_eq!(kept["credentials_enable_service"], false);
    assert_eq!(kept["autofill"]["profile_enabled"], false);
}

/// Watches the front app (by pid) every 40 ms, for as long as the test runs.
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

#[tokio::test]
async fn the_core_reads_frames_and_acts_by_ref_with_trusted_input() {
    let (_dir, state, _area) = test_state();
    let mut session = Session::start_with(&latch(), &state, "b2", Duration::from_secs(120))
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
    assert_eq!(clicked["resolved_by"], "ref");
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

#[tokio::test]
async fn frames_that_come_and_go_while_a_page_loads_break_no_call() {
    let (_dir, state, _area) = test_state();
    let mut session = Session::start_with(&latch(), &state, "b2-churn", Duration::from_secs(120))
        .await
        .unwrap();
    let url = test_site().replace("frames.html", "churn.html");
    let mut failures = Vec::new();
    for round in 0..3 {
        // Each load, and each reading after it, while cross-site frames
        // attach and go away underneath.
        let opened = session.call(json!({"op": "open", "url": url})).await;
        let mut calls = vec![opened];
        for _ in 0..8 {
            calls.push(session.call(json!({"op": "observe"})).await);
            calls.push(session.call(json!({"op": "text"})).await);
        }
        for call in calls {
            if let Err(failure) = call {
                failures.push(format!(
                    "round {round}: {} {}",
                    failure.code, failure.message
                ));
            }
        }
    }
    let text = session.call(json!({"op": "text"})).await.unwrap().result;
    println!("{}", text["text"].as_str().unwrap_or_default().trim());
    assert!(failures.is_empty(), "{failures:#?}");
    session.close().await.unwrap();
}

#[tokio::test]
async fn a_frames_target_under_a_parent_page_layer_is_refused() {
    let (_dir, state, _area) = test_state();
    let mut session = Session::start_with(&latch(), &state, "b2-layer", Duration::from_secs(120))
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

#[tokio::test]
async fn eval_upload_and_download_move_values_and_files() {
    let (_dir, state, _area) = test_state();
    let mut session = Session::start_with(&latch(), &state, "b3", Duration::from_secs(120))
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
        );
        browsers.test = Some((latch(), Vec::new()));
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
        let (result, audit) = run_as(&self.browsers, caller, command, name, args).await;
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
    caller: &str,
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

/// `act` with one step written as the client writes it.
fn act(line: &str) -> serde_json::Value {
    json!({
        "request": format!("r{:012x}", rand::random::<u64>() & 0xffff_ffff_ffff),
        "list": false,
        "steps": super::client::read_steps(line).unwrap(),
    })
}

#[tokio::test]
async fn sessions_on_one_profile_share_logins_and_nothing_else() {
    let (_dir, state, _area) = test_state();
    let base = test_site()
        .split("/frames.html")
        .next()
        .unwrap()
        .to_string();
    let mut h = Host::new(&state);
    let other = "another-caller";
    let fronted = front_apps();

    // Two callers open the same profile: two sessions, one browser.
    let (a, page_a) = h
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

    // A dialog is its own session's: the other reads on and has none.
    let shown = find(&page_a, "button", "Show alert");
    let clicked = h
        .call(CALLER, "act", &a, act(&format!("click {shown}")))
        .await
        .unwrap();
    println!("dialog in a: {}", clicked["events"]);
    assert_eq!(clicked["events"][0]["event"], "dialog");
    let seen = h.call(other, "observe", &b, json!({})).await.unwrap();
    assert_eq!(seen["title"], "latch files test");
    let none = h
        .call(other, "dialog", &b, json!({"accept": true}))
        .await
        .unwrap_err();
    println!("dialog accept in b: {}", none.code);
    assert_eq!(none.code, "no_dialog");
    h.call(CALLER, "dialog", &a, json!({"accept": true}))
        .await
        .unwrap();

    // A popup belongs to the session whose page opened it.
    let page_a = h.call(CALLER, "observe", &a, json!({})).await.unwrap();
    let link = find(&page_a, "link", "Open next in a new tab");
    let clicked = h
        .call(CALLER, "act", &a, act(&format!("click {link}")))
        .await
        .unwrap();
    println!("popup in a: {}", clicked["events"]);
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
    assert_eq!(tabs(&seen_a), ["alert.html", "next.html"]);
    assert_eq!(seen_a["tabs"][1]["opener"], "t1");
    assert_eq!(tabs(&seen_b), ["files.html"]);

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
            CALLER,
            "act",
            &a,
            act(&format!("click {}", links[0]))
        ),
        run_as(
            &h.browsers,
            other,
            "act",
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
            caller,
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
        "act",
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
        ("act", act("scroll down")),
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

    // Nothing above brought the browser to the front.
    let fronted = fronted.lock().unwrap().clone();
    println!("front apps seen: {fronted:?}; engine {engine}");
    assert!(!fronted.contains(&engine));

    h.call(CALLER, "close", &c, json!({})).await.unwrap();
    h.call(other, "close", &b, json!({})).await.unwrap();
    println!(
        "runtime after the last close: alive {}",
        entry_b.runtime.alive()
    );
    assert!(!entry_b.runtime.alive());
}

#[tokio::test]
async fn sessions_warn_on_a_shared_site_and_a_reserved_one_is_refused() {
    let (_dir, state, _area) = test_state();
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
        .call(other, "act", &b, act(&format!("click {shown}")))
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
        .call(other, "act", &b, act(&format!("click {shown}")))
        .await
        .unwrap();
    println!("after close: {}", clicked["events"]);
    h.call(other, "dialog", &b, json!({"accept": true}))
        .await
        .unwrap();
    h.call(other, "close", &b, json!({})).await.unwrap();
}

#[tokio::test]
async fn one_sessions_wait_does_not_hold_the_browser_for_another() {
    let (_dir, state, _area) = test_state();
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
            CALLER,
            "act",
            &a,
            act("scroll down --expect-text \"never on this page\" --within 4"),
        )
        .await;
        (result, started.elapsed())
    };
    let quick = async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let mut done = Vec::new();
        for line in ["scroll down", "scroll up", "scroll down"] {
            let (result, _) = run_as(&h.browsers, CALLER, "act", &b, act(line)).await;
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
    assert!(quick.last().unwrap() < &Duration::from_secs(3));
    assert!(slow_took >= Duration::from_secs(4));
    for id in [&a, &b] {
        h.call(CALLER, "close", id, json!({})).await.unwrap();
    }
}

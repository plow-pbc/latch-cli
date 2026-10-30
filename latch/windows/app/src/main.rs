//! Plow Latch for Windows: the window. Like the Mac app's (`latch/gui`), it
//! keeps no logic of its own: every screen runs the `latch.exe` beside it,
//! with the same arguments, and reads its JSON. A command answers once
//! (`run`); one that streams (`login --events`, `status --local --follow`)
//! sends each line it prints as an event (`stream`).
#![windows_subsystem = "windows"]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

use serde_json::{Value, json};
use tauri::{AppHandle, Emitter, State};

/// latch is a console program; started from a window it would open a
/// console of its own.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The latch beside this app, or `LATCH_EXE` while the app is developed.
fn latch() -> PathBuf {
    if let Some(path) = std::env::var_os("LATCH_EXE") {
        return path.into();
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("latch.exe")))
        .unwrap_or_else(|| "latch.exe".into())
}

fn start(args: &[String]) -> Result<Child, Value> {
    Command::new(latch())
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|err| failure("no_binary", &format!("{}: {err}", latch().display())))
}

/// A failure shaped as latch's own, for the window's one error path.
fn failure(code: &str, message: &str) -> Value {
    json!({ "error": { "code": code, "message": message } })
}

/// The last JSON line of what latch printed, which is its answer (or, on
/// stderr, its failure).
fn last_json(text: &str) -> Option<Value> {
    text.lines()
        .rev()
        .find_map(|line| serde_json::from_str(line).ok())
}

/// Runs one latch command, with `input` on its stdin: its answer, or its
/// failure.
#[tauri::command]
async fn run(args: Vec<String>, input: Option<String>) -> Result<Value, Value> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut child = start(&args)?;
        let mut stdin = child.stdin.take().expect("piped stdin");
        if let Some(input) = input {
            stdin
                .write_all(input.as_bytes())
                .map_err(|err| failure("io", &err.to_string()))?;
        }
        drop(stdin);
        let output = child
            .wait_with_output()
            .map_err(|err| failure("io", &err.to_string()))?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        if output.status.success() {
            return Ok(last_json(&stdout).unwrap_or(Value::Null));
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(last_json(&stderr).unwrap_or_else(|| failure("latch_failed", stderr.trim())))
    })
    .await
    .map_err(|err| failure("io", &err.to_string()))?
}

/// The streams the window follows, by its id for each, so it can stop one.
#[derive(Default)]
struct Streams(Mutex<HashMap<String, Child>>);

/// Starts a streaming command: each JSON line it prints is a `latch-line`
/// event, and its end a `latch-end` event with its failure, if it had one.
/// Its stdin stays open: latch takes that pipe closing as the window gone.
#[tauri::command]
fn stream(
    app: AppHandle,
    streams: State<Streams>,
    id: String,
    args: Vec<String>,
) -> Result<(), Value> {
    let mut child = start(&args)?;
    let stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    if let Some(mut old) = streams
        .0
        .lock()
        .expect("streams lock")
        .insert(id.clone(), child)
    {
        let _ = old.kill();
    }
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(line) = serde_json::from_str::<Value>(&line) {
                let _ = app.emit("latch-line", json!({ "id": id, "line": line }));
            }
        }
        let mut said = String::new();
        let _ = stderr.read_to_string(&mut said);
        let _ = app.emit(
            "latch-end",
            json!({ "id": id, "failure": last_json(&said) }),
        );
    });
    Ok(())
}

/// Ends a stream the window no longer follows.
#[tauri::command]
fn stop(streams: State<Streams>, id: String) {
    if let Some(mut child) = streams.0.lock().expect("streams lock").remove(&id) {
        let _ = child.kill();
    }
}

/// `text` as a QR code, in SVG: the login text a phone's camera prepares.
#[tauri::command]
fn qr(text: String) -> Result<String, Value> {
    let code =
        qrcode::QrCode::new(text.as_bytes()).map_err(|err| failure("qr", &err.to_string()))?;
    Ok(code
        .render::<qrcode::render::svg::Color>()
        .quiet_zone(true)
        .min_dimensions(220, 220)
        .build())
}

/// The Plow the app logs in to: production, unless `PLOW_LATCH_API_URL`
/// names a test stack.
#[tauri::command]
fn api_url() -> Option<String> {
    std::env::var("PLOW_LATCH_API_URL")
        .ok()
        .filter(|url| !url.is_empty())
}

fn main() {
    tauri::Builder::default()
        .manage(Streams::default())
        .invoke_handler(tauri::generate_handler![run, stream, stop, qr, api_url])
        .run(tauri::generate_context!())
        .expect("the window runs");
}

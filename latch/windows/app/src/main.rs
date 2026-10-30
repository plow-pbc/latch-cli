//! Plow Latch for Windows: the window. Like the Mac app's (`latch/gui`), it
//! keeps no logic of its own: every screen runs the `latch.exe` beside it,
//! with the same arguments, and reads its JSON. A command answers once
//! (`run`); one that streams (`login --events`, `status --local --follow`)
//! sends each line it prints as an event (`stream`). A test Plow is given
//! as latch's own `PLOW_API_URL`, which the latch it runs inherits.
#![windows_subsystem = "windows"]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

use serde_json::{Value, json};
use tauri::{AppHandle, Emitter, Manager, State};

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

/// Runs one latch command, with `input` on its stdin: its answer (with
/// `lines`, every JSON line it printed, as `audit`'s pages have many; with
/// `text`, what it printed as it printed it, as `--pretty`'s indented
/// JSON), or its failure.
#[tauri::command]
async fn run(
    args: Vec<String>,
    input: Option<String>,
    lines: Option<bool>,
    text: Option<bool>,
) -> Result<Value, Value> {
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
        if output.status.success() && text == Some(true) {
            return Ok(Value::String(stdout.into_owned()));
        }
        if output.status.success() && lines == Some(true) {
            let all = stdout.lines().filter_map(|line| serde_json::from_str(line).ok());
            return Ok(Value::Array(all.collect()));
        }
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

/// Runs a latch command that writes a file, its `{file}` argument a path in
/// the temp folder, and answers with that file as a data URL (`audit shot`'s
/// screenshot, `agents card`'s contact card); the file goes once read.
#[tauri::command]
async fn run_file(args: Vec<String>, kind: String) -> Result<String, Value> {
    use base64::Engine;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    let path = std::env::temp_dir().join(format!("latch-window-{}-{stamp}.{kind}", std::process::id()));
    let file = path.to_string_lossy().into_owned();
    let args = args
        .into_iter()
        .map(|arg| if arg == "{file}" { file.clone() } else { arg })
        .collect();
    let ran = run(args, None, None, None).await;
    let read = std::fs::read(&path);
    let _ = std::fs::remove_file(&path);
    ran?;
    let bytes = read.map_err(|err| failure("io", &err.to_string()))?;
    let mime = match kind.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "vcf" => "text/vcard",
        _ => "application/octet-stream",
    };
    Ok(format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes)))
}

/// Opens a web link in the default browser (a sign-in page): http(s) only.
#[tauri::command]
fn open_url(url: String) -> Result<(), Value> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(failure("bad_url", &url));
    }
    Command::new("rundll32")
        .args(["url.dll,FileProtocolHandler", &url])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map(|_| ())
        .map_err(|err| failure("io", &err.to_string()))
}

/// The native Open dialog: the path picked, or nothing if it was cancelled.
#[tauri::command]
async fn pick_file(window: tauri::Window, title: String, name: String, extensions: Vec<String>) -> Option<String> {
    rfd::AsyncFileDialog::new()
        .set_title(title)
        .add_filter(name, &extensions)
        .set_parent(&window)
        .pick_file()
        .await
        .map(|file| file.path().display().to_string())
}

/// Whether the window shows the Mac app's snapshot fixtures instead of
/// asking latch: a debug build started with `LATCH_APP_FAKE`, for stills
/// beside the Mac's (ui/fake.js).
#[tauri::command]
fn fake_mode() -> bool {
    cfg!(debug_assertions) && std::env::var_os("LATCH_APP_FAKE").is_some()
}

/// WebView2 keeps no form data and no passwords of its own: a vault item's
/// fields are typed in this window, and general autofill is on by default.
fn without_autofill(webview: &tauri::webview::PlatformWebview) -> windows_core::Result<()> {
    use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Settings4;
    use windows_core::Interface;
    // SAFETY: COM calls on the window's own live controller, on its thread.
    unsafe {
        let settings: ICoreWebView2Settings4 = webview.controller().CoreWebView2()?.Settings()?.cast()?;
        settings.SetIsGeneralAutofillEnabled(false.into())?;
        settings.SetIsPasswordAutosaveEnabled(false.into())?;
    }
    Ok(())
}

fn main() {
    tauri::Builder::default()
        .manage(Streams::default())
        .setup(|app| {
            // The window is made here, not from the config alone: a debug
            // build started with LATCH_APP_DEVTOOLS opens WebView2's
            // DevTools protocol on loopback, for driving the window in
            // tests (WebView2 ignores its environment variable under Tauri).
            let config = app.config().app.windows.first().cloned().expect("the window's config");
            let mut made = tauri::WebviewWindowBuilder::from_config(app, &config)?;
            if cfg!(debug_assertions) && std::env::var_os("LATCH_APP_DEVTOOLS").is_some() {
                made = made.additional_browser_args(
                    "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection \
                     --remote-debugging-port=9222 --remote-debugging-address=127.0.0.1",
                );
            }
            made.build()?;
            if let Some(window) = app.get_webview_window("main") {
                window.with_webview(|webview| {
                    let _ = without_autofill(&webview);
                })?;
                // Fake mode only: `LATCH_APP_STILL` is the address's query
                // (`place=vault&host=pi-kitchen`), for stills beside the Mac's.
                if let (true, Ok(still)) = (fake_mode(), std::env::var("LATCH_APP_STILL")) {
                    let query = serde_json::to_string(&still).unwrap_or_default();
                    window.eval(&format!("if (!location.search) location.search = {query}"))?;
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![run, run_file, pick_file, open_url, stream, stop, qr, fake_mode])
        .build(tauri::generate_context!())
        .expect("the window builds")
        .run(|app, event| {
            // A login still waiting for its text would log this PC in after
            // the window is gone: every stream ends with the window.
            if let tauri::RunEvent::Exit = event {
                let streams = app.state::<Streams>();
                for (_, mut child) in streams.0.lock().expect("streams lock").drain() {
                    let _ = child.kill();
                }
            }
        });
}

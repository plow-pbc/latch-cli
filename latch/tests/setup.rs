//! `latch setup` at a terminal (a pty), against a stand-in latch that
//! answers each command the way the real one does, keeps what it was told in
//! files, and logs every call. The stand-in lets these tests cover every
//! answer without a Plow, a host or System Settings.

use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;

/// The stand-in. Its state folder (`$STUB`) holds `peer` (logged in),
/// `ready` (the host answers), `enabled-NAME`, and `enable-NAME`: one answer
/// per call to `plugins enable NAME`, `ok`, `not_granted`, `unavailable` or
/// `fail`, the last one repeating. With `taken`, login finds this machine's
/// name taken and asks, as the real one does, whether to replace it and
/// else for a new name; it keeps the answers in `login-answers`.
const STUB: &str = r#"#!/bin/sh
[ "$1" = --state-dir ] && shift 2
echo "${LATCH_HOST:+LATCH_HOST=$LATCH_HOST }$*" >> "$STUB/calls"
state() { if [ -f "$STUB/$1" ]; then echo true; else echo false; fi; }
case "$*" in
  "status --local")
    if [ -f "$STUB/peer" ]; then login='{"kind":"device"}' name='"mac"'; else login=null name=null; fi
    plugins='['; sep=''
    for plugin in google messages contacts; do
      if [ -f "$STUB/enabled-$plugin" ]; then s=enabled; else s=disabled; fi
      plugins="$plugins$sep{\"name\":\"$plugin\",\"state\":\"$s\"}"; sep=,
    done
    echo "{\"login\":$login,\"this_machine\":{\"name\":$name,\"hosting\":{\"running\":$(state ready),\"ready\":$(state ready)}},\"plugins\":$plugins]}" ;;
  login)
    if [ -f "$STUB/taken" ]; then
      printf 'A machine named "mac" already exists. Replace it? [y/N] ' >&2
      read -r answer; echo "$answer" >> "$STUB/login-answers"
      if [ "$answer" != y ]; then
        printf 'New name for this machine: ' >&2
        read -r answer; echo "$answer" >> "$STUB/login-answers"
      fi
    fi
    # A login by text ends in a person's line; one that asks no one
    # (`json-login`) answers its JSON; one by text that fails
    # (`login-fails`) says why in a plain line.
    if [ -f "$STUB/login-fails" ]; then
      echo 'Waiting for your text…' >&2
      echo 'the code expired before it was texted; run `latch login` again' >&2
      exit 1
    fi
    if [ -f "$STUB/json-login" ]; then echo '{"peer":{"name":"mac","key":"k","kind":"device"},"key_created":true}'
    else echo "Logged in as mac." >&2; fi
    touch "$STUB/peer" ;;
  "serve -d"|"serve -d --again") touch "$STUB/ready"; echo '{"pid":4242}' ;;
  "plugins enable "*)
    name=$3
    answer=$(head -n 1 "$STUB/enable-$name" 2>/dev/null || echo ok)
    if [ "$(wc -l < "$STUB/enable-$name" 2>/dev/null || echo 0)" -gt 1 ]; then
      tail -n +2 "$STUB/enable-$name" > "$STUB/next" && mv "$STUB/next" "$STUB/enable-$name"
    fi
    case $answer in
      ok) touch "$STUB/enabled-$name"; echo "{\"plugin\":\"$name\",\"state\":\"enabled\"}" ;;
      not_granted)
        echo "{\"error\":{\"code\":\"needs_setup\",\"message\":\"no Full Disk Access\",\"access\":\"not_granted\",\"grant\":\"/Applications/Plow Latch.app\",\"settings\":\"x-apple.systempreferences:test\"}}" >&2
        exit 1 ;;
      stopped) exit 0 ;;
      unavailable) echo "{\"error\":{\"code\":\"unavailable\",\"message\":\"$name is unavailable on this host\"}}" >&2; exit 1 ;;
      *) echo "{\"error\":{\"code\":\"google_not_connected\",\"message\":\"Plow has no Google account\"}}" >&2; exit 1 ;;
    esac ;;
  *) echo "{\"error\":{\"code\":\"usage\",\"message\":\"stub: $*\"}}" >&2; exit 2 ;;
esac
"#;

struct Stub {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl Stub {
    fn new() -> Stub {
        let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.dev/tests");
        fs::create_dir_all(&parent).unwrap();
        let dir = tempfile::tempdir_in(parent).unwrap();
        let root = dir.path().canonicalize().unwrap();
        fs::create_dir_all(root.join("stub")).unwrap();
        fs::create_dir_all(root.join("bin")).unwrap();
        for (name, body) in [
            ("latch-stub", STUB),
            // `open` only records what it was asked to open.
            ("bin/open", "#!/bin/sh\necho \"$*\" >> \"$STUB/opened\"\n"),
        ] {
            fs::write(root.join(name), body).unwrap();
            fs::set_permissions(root.join(name), fs::Permissions::from_mode(0o755)).unwrap();
        }
        Stub { _dir: dir, root }
    }

    fn file(&self, name: &str, body: &str) {
        fs::write(self.root.join("stub").join(name), body).unwrap();
    }

    /// `name` enabled, as the stand-in's `status --local` lists it.
    #[cfg(target_os = "macos")]
    fn enable(&self, name: &str) {
        self.file(&format!("enabled-{name}"), "");
    }

    fn has(&self, name: &str) -> bool {
        self.root.join("stub").join(name).exists()
    }

    fn read(&self, name: &str) -> String {
        fs::read_to_string(self.root.join("stub").join(name)).unwrap_or_default()
    }

    /// `latch setup` at a terminal with `typed` already typed: its exit
    /// code and everything the terminal showed.
    fn setup(&self, typed: &str) -> (Option<i32>, String) {
        use std::os::fd::FromRawFd;
        let (mut main, mut side) = (0, 0);
        // SAFETY: openpty fills in two new descriptors, which we then own.
        let opened = unsafe {
            libc::openpty(
                &mut main,
                &mut side,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(opened, 0);
        let (mut main, side) =
            unsafe { (fs::File::from_raw_fd(main), fs::File::from_raw_fd(side)) };
        let path = format!(
            "{}:{}",
            self.root.join("bin").display(),
            std::env::var("PATH").unwrap()
        );
        let mut command = Command::new(env!("CARGO_BIN_EXE_latch"));
        command
            .args(["--state-dir"])
            .arg(self.root.join("state"))
            .arg("setup")
            .env("LATCH_SETUP_LATCH", self.root.join("latch-stub"))
            .env("STUB", self.root.join("stub"))
            // As if the owner's shell named another host for other commands.
            .env("LATCH_HOST", "elsewhere")
            .env("PATH", path)
            .stdin(side.try_clone().unwrap())
            .stdout(side.try_clone().unwrap())
            .stderr(side.try_clone().unwrap());
        // The pty is its controlling terminal, as a person's is: when the
        // test ends and closes it, the kernel hangs setup up.
        // SAFETY: setsid and ioctl only, between fork and exec.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn().unwrap();
        // Every copy of the terminal's side must close, the command's too,
        // or reading the other side never ends once setup does.
        drop(command);
        drop(side);
        main.write_all(typed.as_bytes()).unwrap();
        let mut reading = main.try_clone().unwrap();
        let shown = thread::spawn(move || {
            let mut shown = Vec::new();
            let _ = reading.read_to_end(&mut shown);
            String::from_utf8_lossy(&shown).to_string()
        });
        // A question nobody answers would wait for ever: stop it instead.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let code = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status.code();
            }
            if std::time::Instant::now() > deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                drop(main);
                let shown = shown.join().unwrap();
                panic!("setup still waiting after 60 s; it showed: {shown}");
            }
            thread::sleep(std::time::Duration::from_millis(50));
        };
        drop(main);
        (code, shown.join().unwrap())
    }
}

#[test]
fn a_fresh_run_sets_everything_up_then_starts_latch() {
    let stub = Stub::new();
    // Enter takes each default: Google yes, Messages no, start yes.
    let (code, shown) = stub.setup("\n\n\n");
    assert_eq!(code, Some(0), "{shown}");
    for line in [
        "→ Log in: latch login",
        "Connect Google? [Y/n]",
        "→ Connect Google: latch plugins enable google",
        "✓ Google is connected.",
        "Start latch now? [Y/n]",
        "→ Start latch: latch serve -d",
        "✓ latch is running.",
    ] {
        assert!(shown.contains(line), "{line}: {shown}");
    }
    let calls = stub.read("calls");
    assert!(calls.contains("\nlogin\n"), "{calls}");
    // The host starts last: no step before it needs one.
    let (google, serve) = (
        calls.find("plugins enable google").unwrap(),
        calls.find("serve -d").unwrap(),
    );
    assert!(google < serve, "{calls}");
    assert!(!calls.contains("LATCH_HOST"), "{calls}");
    assert!(stub.has("enabled-google"));
    assert!(!stub.has("enabled-messages"));
    #[cfg(not(target_os = "macos"))]
    assert!(!shown.contains("Messages"), "{shown}");
}

#[test]
fn login_gets_the_answers_to_its_own_questions() {
    let stub = Stub::new();
    stub.file("taken", "");
    // Login's: keep the other machine, call this one "studio". Then
    // setup's: Google yes, Messages no, start no.
    let (code, shown) = stub.setup("n\nstudio\n\nn\nn\n");
    assert_eq!(code, Some(0), "{shown}");
    assert_eq!(stub.read("login-answers"), "n\nstudio\n", "{shown}");
    assert!(shown.contains("✓ Google is connected."), "{shown}");
}

#[test]
fn a_rerun_skips_every_step_already_done() {
    let stub = Stub::new();
    for done in [
        "peer",
        "ready",
        "enabled-google",
        "enabled-messages",
        "enabled-contacts",
    ] {
        stub.file(done, "");
    }
    let (code, shown) = stub.setup("");
    assert_eq!(code, Some(0), "{shown}");
    assert!(shown.contains("✓ Logged in as mac."), "{shown}");
    assert!(shown.contains("✓ Google is connected."), "{shown}");
    assert!(shown.contains("✓ latch is running."), "{shown}");
    assert!(!shown.contains('?'), "nothing asked: {shown}");
    let calls = stub.read("calls");
    for changed in ["login", "serve", "plugins enable"] {
        assert!(!calls.contains(changed), "{changed}: {calls}");
    }
}

#[test]
fn every_no_leaves_its_step_for_later() {
    let stub = Stub::new();
    stub.file("peer", "");
    let (code, shown) = stub.setup("n\nn\nn\n");
    assert_eq!(code, Some(0), "{shown}");
    assert!(
        shown.contains("Skipped; later: `latch plugins enable google`."),
        "{shown}"
    );
    assert!(
        shown.contains(
            "Not started; later: `latch serve`, or `latch serve -d` to run it in the background."
        ),
        "{shown}"
    );
    let calls = stub.read("calls");
    assert!(
        !calls.contains("serve") && !calls.contains("plugins enable"),
        "{calls}"
    );
}

/// A Google sign-in whose wait was stopped exits 0 with nothing on stdout:
/// that is not Google turned on.
#[test]
fn a_stopped_google_sign_in_is_not_called_connected() {
    let stub = Stub::new();
    stub.file("peer", "");
    stub.file("enable-google", "stopped\n");
    let (code, shown) = stub.setup("y\nn\nn\n");
    assert_eq!(code, Some(0), "{shown}");
    assert!(!shown.contains("✓ Google is connected."), "{shown}");
    assert!(shown.contains("✗ Google is not on yet."), "{shown}");
}

#[test]
fn a_failed_step_says_why_and_the_next_one_still_runs() {
    let stub = Stub::new();
    stub.file("peer", "");
    stub.file("enable-google", "fail\n");
    let (code, shown) = stub.setup("y\nn\nn\n");
    assert_eq!(code, Some(0), "{shown}");
    // The enable says why itself, at the same terminal.
    assert!(shown.contains("Plow has no Google account"), "{shown}");
    assert!(shown.contains("✗ Google is not on yet."), "{shown}");
    assert!(
        shown.contains("Try again later: latch plugins enable google"),
        "{shown}"
    );
    assert!(shown.contains("Start latch now? [Y/n]"), "{shown}");
}

/// A login that fails: what it said stays on the terminal, and setup,
/// going by its exit status, says the other steps wait.
#[test]
fn a_failed_login_leaves_its_line_and_the_other_steps_wait() {
    let stub = Stub::new();
    stub.file("login-fails", "");
    let (code, shown) = stub.setup("");
    assert_eq!(code, Some(0), "{shown}");
    assert!(shown.contains("Waiting for your text…"), "{shown}");
    assert!(
        shown.contains("the code expired before it was texted"),
        "{shown}"
    );
    assert!(
        shown.contains("Not logged in, so the other steps wait"),
        "{shown}"
    );
    let calls = stub.read("calls");
    assert!(
        !calls.contains("enable") && !calls.contains("serve"),
        "{calls}"
    );
}

/// A login that asks no one answers JSON: setup reads it and says so in
/// its own line, and never shows the JSON.
#[test]
fn a_login_that_asks_no_one_is_read_not_shown() {
    let stub = Stub::new();
    stub.file("json-login", "");
    let (code, shown) = stub.setup("n\nn\nn\n");
    assert_eq!(code, Some(0), "{shown}");
    assert!(shown.contains("✓ Logged in as mac."), "{shown}");
    assert!(!shown.contains("{\"peer\""), "{shown}");
}

#[cfg(target_os = "macos")]
#[test]
fn messages_and_contacts_are_confirmed_once_granted() {
    let stub = Stub::new();
    stub.file("peer", "");
    // Google was turned on before, and latch runs, so setup asks nothing
    // after this step (a line typed during the wait is not an answer).
    stub.enable("google");
    stub.file("ready", "");
    // Not granted at first; granted on the host's next check.
    stub.file("enable-messages", "not_granted\nok\n");
    stub.file("enable-contacts", "not_granted\nok\n");
    let (code, shown) = stub.setup("y\n");
    assert_eq!(code, Some(0), "{shown}");
    assert!(
        shown.contains("Turn on Full Disk Access for /Applications/Plow Latch.app"),
        "{shown}"
    );
    assert!(
        shown.contains("✓ Messages is on (latch plugins enable messages)."),
        "{shown}"
    );
    assert!(
        shown.contains("✓ Contacts is on (latch plugins enable contacts)."),
        "{shown}"
    );
    assert_eq!(
        stub.read("opened"),
        "x-apple.systempreferences:test\n-R /Applications/Plow Latch.app\n"
    );
    let calls = stub.read("calls");
    assert_eq!(
        calls.matches("plugins enable messages").count(),
        2,
        "{calls}"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn s_skips_the_full_disk_access_wait_and_leaves_it_for_later() {
    let stub = Stub::new();
    stub.file("peer", "");
    stub.file("ready", "");
    stub.enable("google");
    stub.file("enable-messages", "not_granted\n");
    stub.file("enable-contacts", "not_granted\n");
    let (code, shown) = stub.setup("y\ns\n");
    assert_eq!(code, Some(0), "{shown}");
    assert!(
        shown.contains("Skipped; finish later with `latch plugins enable messages`"),
        "{shown}"
    );
    assert!(!stub.has("enabled-messages") && !stub.has("enabled-contacts"));
    assert!(shown.contains("✓ latch is running."), "{shown}");
}

#[test]
fn a_host_that_ran_here_before_starts_again_with_its_flags() {
    let stub = Stub::new();
    stub.file("peer", "");
    fs::create_dir_all(stub.root.join("state")).unwrap();
    fs::write(
        stub.root.join("state/serve-args.json"),
        r#"["--deny-exec"]"#,
    )
    .unwrap();
    // Google no, on macOS Messages no, start yes.
    let typed = if cfg!(target_os = "macos") {
        "n\nn\n\n"
    } else {
        "n\n\n"
    };
    let (code, shown) = stub.setup(typed);
    assert_eq!(code, Some(0), "{shown}");
    assert!(
        shown.contains("→ Start latch: latch serve -d --again"),
        "{shown}"
    );
    let calls = stub.read("calls");
    assert!(calls.contains("serve -d --again\n"), "{calls}");
}

#[cfg(target_os = "macos")]
#[test]
fn a_connector_failing_for_another_reason_is_reported_not_waited_for() {
    let stub = Stub::new();
    stub.file("peer", "");
    stub.file("ready", "");
    stub.enable("google");
    stub.file("enable-messages", "not_granted\nok\n");
    stub.file("enable-contacts", "unavailable\n");
    let (code, shown) = stub.setup("y\n");
    assert_eq!(code, Some(0), "{shown}");
    for line in [
        "✗ contacts is unavailable on this host",
        "  Try again later: latch plugins enable contacts",
    ] {
        assert!(shown.contains(line), "{line}: {shown}");
    }
    assert!(
        shown.contains("✓ Messages is on (latch plugins enable messages)."),
        "{shown}"
    );
    let calls = stub.read("calls");
    assert_eq!(
        calls.matches("plugins enable contacts").count(),
        1,
        "{calls}"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn a_failure_on_a_later_check_is_reported_not_waited_out() {
    let stub = Stub::new();
    stub.file("peer", "");
    stub.file("ready", "");
    stub.enable("google");
    stub.file("enable-messages", "not_granted\nunavailable\n");
    stub.file("enable-contacts", "not_granted\nok\n");
    let (code, shown) = stub.setup("y\n");
    assert_eq!(code, Some(0), "{shown}");
    for line in [
        "✗ messages is unavailable on this host",
        "  Try again later: latch plugins enable messages",
    ] {
        assert!(shown.contains(line), "{line}: {shown}");
    }
    assert!(
        shown.contains("✓ Contacts is on (latch plugins enable contacts)."),
        "{shown}"
    );
    assert!(!shown.contains("Not granted yet"), "{shown}");
}

/// `latch setup --status` on `state`, without a terminal: its JSON steps,
/// by name.
fn status(state: &Path) -> serde_json::Map<String, serde_json::Value> {
    let output = Command::new(env!("CARGO_BIN_EXE_latch"))
        .arg("--state-dir")
        .arg(state)
        .args(["setup", "--status"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    let answer: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    answer["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|step| (step["step"].as_str().unwrap().to_string(), step.clone()))
        .collect()
}

/// A state folder logged in as `kind`, with `files` written into it.
fn logged_in(stub: &Stub, kind: &str, files: &[(&str, &str)]) -> PathBuf {
    let state = stub.root.join("state");
    fs::create_dir_all(&state).unwrap();
    let peer = serde_json::json!({"name": "mac", "key": "k", "kind": kind,
        "relay_url": null, "last_seen_at": null});
    let config = serde_json::json!({"api_url": "http://127.0.0.1:9", "peer": peer});
    fs::write(
        state.join("latch.json"),
        r#"{"format":2,"written_by":"test"}"#,
    )
    .unwrap();
    fs::write(state.join("config.json"), config.to_string()).unwrap();
    for (name, body) in files {
        fs::write(state.join(name), body).unwrap();
    }
    state
}

/// Every file in `dir` and what it holds.
fn contents(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut found: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            let body = fs::read(&path).unwrap();
            (path, body)
        })
        .collect();
    found.sort();
    found
}

#[test]
fn status_on_a_fresh_state_asks_for_a_login_first_and_creates_nothing() {
    let stub = Stub::new();
    let state = stub.root.join("state");
    let steps = status(&state);
    assert_eq!(steps["login"]["state"], "needed");
    assert_eq!(
        steps["login"]["args"],
        serde_json::json!(["login", "--events"])
    );
    assert_eq!(steps["google"]["state"], "blocked");
    assert_eq!(steps["google"]["args"], serde_json::Value::Null);
    assert_eq!(steps["host"]["state"], "blocked");
    assert!(!state.exists());
}

#[test]
fn status_reads_the_saved_connectors_and_serve_flags_and_changes_nothing() {
    let stub = Stub::new();
    // Messages is a connector only macOS builds have.
    let plugins = if cfg!(target_os = "macos") {
        r#"{"google":{"state":"enabled"},"messages":{"state":"needs_setup","reason":"this host has no Full Disk Access"}}"#
    } else {
        r#"{"google":{"state":"enabled"}}"#
    };
    let state = logged_in(
        &stub,
        "device",
        &[
            ("plugins.json", plugins),
            ("serve-args.json", r#"["--deny-exec"]"#),
        ],
    );
    let before = contents(&state);
    let steps = status(&state);
    assert_eq!(steps["login"]["state"], "done");
    assert_eq!(steps["login"]["reason"], "Logged in as mac.");
    // Enabled here, but Plow cannot be asked whether Google is still
    // connected there: blocked, never guessed.
    let google = &steps["google"];
    assert_eq!(google["state"], "blocked");
    assert_eq!(google["enabled"], true);
    assert_eq!(google["ready"], false);
    assert_eq!(google["args"], serde_json::Value::Null);
    assert!(
        google["reason"]
            .as_str()
            .unwrap()
            .starts_with("Plow could not say whether Google is connected"),
        "{google}"
    );
    assert_eq!(
        steps["host"]["args"],
        serde_json::json!(["serve", "-d", "--again"])
    );
    if cfg!(target_os = "macos") {
        let messages = &steps["messages"];
        assert_eq!(messages["state"], "blocked");
        assert!(
            messages["reason"]
                .as_str()
                .unwrap()
                .contains("Full Disk Access")
        );
        assert_eq!(
            messages["args"],
            serde_json::json!(["plugins", "enable", "messages"])
        );
        assert_eq!(steps["contacts"]["state"], "needed");
    } else {
        assert!(!steps.contains_key("messages"));
    }
    assert_eq!(contents(&state), before);
}

#[test]
fn status_on_an_agent_login_leaves_the_connectors_to_the_owner() {
    let stub = Stub::new();
    let state = logged_in(&stub, "agent", &[]);
    let steps = status(&state);
    assert_eq!(steps["google"]["state"], "blocked");
    assert_eq!(steps["google"]["args"], serde_json::Value::Null);
    assert_eq!(steps["host"]["state"], "needed");
    assert_eq!(steps["host"]["args"], serde_json::json!(["serve", "-d"]));
}

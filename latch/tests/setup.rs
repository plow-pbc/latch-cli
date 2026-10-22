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
/// per call to `plugins enable NAME`, `ok`, `not_granted` or `fail`, the
/// last one repeating.
const STUB: &str = r#"#!/bin/sh
[ "$1" = --state-dir ] && shift 2
echo "$*" >> "$STUB/calls"
state() { if [ -f "$STUB/$1" ]; then echo true; else echo false; fi; }
case "$*" in
  "status --local")
    if [ -f "$STUB/peer" ]; then peer='{"name":"mac","key":"k","kind":"device"}'; else peer=null; fi
    echo "{\"peer\":$peer,\"serve\":{\"running\":$(state ready),\"ready\":$(state ready)}}" ;;
  login) echo "Logged in as mac."; touch "$STUB/peer" ;;
  "serve -d") touch "$STUB/ready"; echo '{"pid":4242}' ;;
  "plugins list")
    out='{"plugins":['; sep=''
    for name in google messages contacts; do
      if [ -f "$STUB/enabled-$name" ]; then s=enabled; else s=disabled; fi
      out="$out$sep{\"name\":\"$name\",\"state\":\"$s\"}"; sep=,
    done
    echo "$out]}" ;;
  "plugins enable "*)
    name=$3
    answer=$(head -n 1 "$STUB/enable-$name" 2>/dev/null || echo ok)
    if [ "$(wc -l < "$STUB/enable-$name" 2>/dev/null || echo 0)" -gt 1 ]; then
      tail -n +2 "$STUB/enable-$name" > "$STUB/next" && mv "$STUB/next" "$STUB/enable-$name"
    fi
    case $answer in
      ok) touch "$STUB/enabled-$name"; echo "{\"plugin\":\"$name\",\"state\":\"enabled\"}" ;;
      not_granted)
        echo "{\"error\":{\"code\":\"needs_setup\",\"message\":\"no Full Disk Access\",\"details\":{\"access\":\"not_granted\",\"grant\":\"/Applications/Plow Latch.app\",\"settings\":\"x-apple.systempreferences:test\"}}}" >&2
        exit 1 ;;
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

    /// `name` enabled, as the stand-in's host lists it and as the host
    /// saves it in the state folder.
    #[cfg(target_os = "macos")]
    fn enable(&self, name: &str) {
        self.file(&format!("enabled-{name}"), "");
        let state = self.root.join("state");
        fs::create_dir_all(&state).unwrap();
        let path = state.join("plugins.json");
        let mut saved: serde_json::Value = fs::read_to_string(&path)
            .map(|text| serde_json::from_str(&text).unwrap())
            .unwrap_or_else(|_| serde_json::json!({}));
        saved[name] = serde_json::json!({"state": "enabled"});
        fs::write(path, saved.to_string()).unwrap();
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
fn a_fresh_run_logs_in_connects_google_and_starts_latch() {
    let stub = Stub::new();
    // Enter takes each default: Google yes, Messages no.
    let (code, shown) = stub.setup("\n\n\n");
    assert_eq!(code, Some(0), "{shown}");
    for line in [
        "→ Log in: latch login",
        "Connect Google? [Y/n]",
        "→ Start latch: latch serve -d",
        "→ Connect Google: latch plugins enable google",
        "✓ Google is connected.",
        "✓ latch is running.",
    ] {
        assert!(shown.contains(line), "{line}: {shown}");
    }
    let calls = stub.read("calls");
    assert!(
        calls.contains("\nlogin\n") && calls.contains("serve -d"),
        "{calls}"
    );
    assert!(stub.has("enabled-google"));
    assert!(!stub.has("enabled-messages"));
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
    for changed in ["login", "serve -d", "plugins enable"] {
        assert!(!calls.contains(changed), "{changed}: {calls}");
    }
}

#[test]
fn every_no_leaves_its_step_for_later_and_starts_nothing() {
    let stub = Stub::new();
    stub.file("peer", "");
    let (code, shown) = stub.setup("n\nn\nn\n");
    assert_eq!(code, Some(0), "{shown}");
    assert!(
        shown.contains("Skipped; later: `latch plugins enable google`."),
        "{shown}"
    );
    assert!(
        shown.contains("Not started; later: `latch serve -d`."),
        "{shown}"
    );
    let calls = stub.read("calls");
    assert!(
        !calls.contains("serve -d") && !calls.contains("plugins enable"),
        "{calls}"
    );
}

#[test]
fn a_failed_step_says_why_and_the_next_one_still_runs() {
    let stub = Stub::new();
    stub.file("peer", "");
    stub.file("enable-google", "fail\n");
    let (code, shown) = stub.setup("y\nn\n");
    assert_eq!(code, Some(0), "{shown}");
    assert!(shown.contains("✗ Plow has no Google account"), "{shown}");
    assert!(
        shown.contains("Try again later: latch plugins enable google"),
        "{shown}"
    );
    assert!(shown.contains("✓ latch is running."), "{shown}");
}

#[test]
fn setup_needs_a_terminal() {
    let stub = Stub::new();
    let output = Command::new(env!("CARGO_BIN_EXE_latch"))
        .arg("setup")
        .env("LATCH_SETUP_LATCH", stub.root.join("latch-stub"))
        .env("STUB", stub.root.join("stub"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("run it at a terminal"));
    assert!(!stub.has("calls"));
}

#[cfg(target_os = "macos")]
#[test]
fn messages_and_contacts_are_confirmed_through_the_host_once_granted() {
    let stub = Stub::new();
    stub.file("peer", "");
    // Google was turned on before; the host is not running now.
    stub.enable("google");
    // Not granted at first; granted on the host's next check.
    stub.file("enable-messages", "not_granted\nok\n");
    stub.file("enable-contacts", "not_granted\nok\n");
    let (code, shown) = stub.setup("y\n");
    assert_eq!(code, Some(0), "{shown}");
    assert!(shown.contains("→ Start latch: latch serve -d"), "{shown}");
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

#[cfg(not(target_os = "macos"))]
#[test]
fn linux_has_no_messages_step() {
    let stub = Stub::new();
    stub.file("peer", "");
    let (code, shown) = stub.setup("y\ny\n");
    assert_eq!(code, Some(0), "{shown}");
    assert!(!shown.contains("Messages"), "{shown}");
    assert!(shown.contains("✓ Google is connected."), "{shown}");
}

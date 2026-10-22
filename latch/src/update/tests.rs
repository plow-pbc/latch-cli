//! Updates from a local release server, signed with a key made for each
//! test; the release's install-binary.sh is a stand-in that writes down
//! what it was given. Nothing here reaches the real releases.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use super::*;

/// A release server: the files under `/latch-cli/`, by path, which a test may
/// change; and how many requests it saw.
struct Server {
    url: String,
    files: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    requests: Arc<Mutex<usize>>,
}

fn serve() -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/latch-cli", listener.local_addr().unwrap());
    let files: Arc<Mutex<HashMap<String, Vec<u8>>>> = Arc::default();
    let requests: Arc<Mutex<usize>> = Arc::default();
    let (shared, seen) = (files.clone(), requests.clone());
    std::thread::spawn(move || {
        for socket in listener.incoming() {
            let mut socket = socket.unwrap();
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut first = String::new();
            reader.read_line(&mut first).unwrap();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" || line.is_empty() {
                    break;
                }
            }
            *seen.lock().unwrap() += 1;
            let path = first.split(' ').nth(1).unwrap_or_default();
            let file = path
                .strip_prefix("/latch-cli/")
                .and_then(|path| shared.lock().unwrap().get(path).cloned());
            let (status, body) = match file {
                Some(body) => ("200 OK", body),
                None => ("404 Not Found", Vec::new()),
            };
            let head = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = socket.write_all(head.as_bytes());
            let _ = socket.write_all(&body);
        }
    });
    Server {
        url,
        files,
        requests,
    }
}

/// A signing key made for one test.
struct Key {
    pair: minisign::KeyPair,
}

impl Key {
    fn new() -> Key {
        Key {
            pair: minisign::KeyPair::generate_unencrypted_keypair().unwrap(),
        }
    }

    fn public(&self) -> PublicKey {
        PublicKey::from_base64(&self.pair.pk.to_base64()).unwrap()
    }

    fn sign(&self, bytes: &[u8]) -> Vec<u8> {
        minisign::sign(Some(&self.pair.pk), &self.pair.sk, bytes, None, None)
            .unwrap()
            .to_string()
            .into_bytes()
    }
}

struct World {
    dir: tempfile::TempDir,
    server: Server,
    key: Key,
    state: StateDir,
}

impl World {
    fn new() -> World {
        let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.dev/tests");
        std::fs::create_dir_all(&parent).unwrap();
        let dir = tempfile::tempdir_in(parent).unwrap();
        let state = StateDir {
            path: dir.path().join("state"),
        };
        std::fs::create_dir_all(&state.path).unwrap();
        World {
            dir,
            server: serve(),
            key: Key::new(),
            state,
        }
    }

    fn source(&self) -> Source {
        Source {
            url: self.server.url.clone(),
            key: self.key.public(),
            signer: None,
        }
    }

    fn put(&self, path: &str, bytes: Vec<u8>) {
        self.server
            .files
            .lock()
            .unwrap()
            .insert(path.to_string(), bytes);
    }

    /// Where the stand-in install-binary.sh writes its arguments.
    fn installed(&self) -> PathBuf {
        self.dir.path().join("installed")
    }

    /// Release `build` (tag v0.1.0-<build>), signed, with this platform's
    /// archive.
    fn release(&self, build: u64) -> Release {
        let release = Release::named(&format!("v0.1.0-{build}")).unwrap();
        let name = format!("latch-{}-{}", release.tag, target().unwrap());
        let made = self.dir.path().join(format!("made-{build}"));
        let folder = made.join(&name);
        std::fs::create_dir_all(&folder).unwrap();
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\necho installing\n",
            self.installed().display()
        );
        std::fs::write(folder.join("install-binary.sh"), script).unwrap();
        std::fs::write(folder.join("latch"), format!("build {build}\n")).unwrap();
        let archive = made.join(format!("{name}.tar.gz"));
        let tar = std::process::Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(&made)
            .arg(&name)
            .status()
            .unwrap();
        assert!(tar.success());
        let archive = std::fs::read(archive).unwrap();
        let sums = format!(
            "{}  {name}.tar.gz\n{}  install.sh\n",
            to_hex(&Sha256::digest(&archive)),
            "0".repeat(64)
        );
        let folder = format!("releases/{}", release.tag);
        self.put(&format!("{folder}/{name}.tar.gz"), archive);
        self.put(
            &format!("{folder}/SHA256SUMS.minisig"),
            self.key.sign(sums.as_bytes()),
        );
        self.put(&format!("{folder}/SHA256SUMS"), sums.into_bytes());
        release
    }

    /// `latest.json`, signed, naming `release`.
    fn latest(&self, release: &Release) {
        let bytes = serde_json::to_vec(release).unwrap();
        self.put("latest.json.minisig", self.key.sign(&bytes));
        self.put("latest.json", bytes);
    }

    fn arguments(&self) -> Option<Vec<String>> {
        let text = std::fs::read_to_string(self.installed()).ok()?;
        Some(text.lines().map(str::to_string).collect())
    }
}

/// A newer build goes in through its own installer, into the folder the
/// running latch is in (here ~/bin, not the installer's ~/.local/bin).
#[tokio::test]
async fn a_newer_build_is_installed_by_its_own_installer_where_latch_runs() {
    let world = World::new();
    let release = world.release(1328);
    world.latest(&release);
    let bin = world.dir.path().join("home/bin");
    let answer = update(
        &world.state,
        &world.source(),
        Some(1327),
        Some(&bin),
        None,
        false,
    )
    .await
    .unwrap();
    assert_eq!(
        answer,
        json!({"updated": true, "from": 1327, "to": {"tag": "v0.1.0-1328", "build": 1328}})
    );
    let state = world.state.path.canonicalize().unwrap();
    let arguments = world.arguments().expect("install-binary.sh ran");
    assert_eq!(
        arguments[..6],
        [
            "--state-dir",
            state.to_str().unwrap(),
            "--bin-dir",
            bin.to_str().unwrap(),
            "--non-interactive",
            "--no-browser"
        ]
    );
    assert!(
        arguments[6].ends_with(&format!("latch-v0.1.0-1328-{}/latch", target().unwrap())),
        "{arguments:?}"
    );
}

/// On Linux, latch is installed where this one runs from; on macOS the
/// installer replaces the app every link leads into.
#[test]
fn an_update_installs_where_this_latch_runs() {
    let here = std::env::current_exe().unwrap();
    if cfg!(target_os = "macos") {
        assert_eq!(bin_dir(), None);
    } else {
        assert_eq!(bin_dir().as_deref(), here.parent());
    }
}

#[tokio::test]
async fn check_says_what_is_newer_and_installs_nothing() {
    let world = World::new();
    let release = world.release(1328);
    world.latest(&release);
    let answer = update(&world.state, &world.source(), Some(1327), None, None, true)
        .await
        .unwrap();
    assert_eq!(
        answer,
        json!({"current": 1327, "latest": {"tag": "v0.1.0-1328", "build": 1328}, "available": true})
    );
    let answer = update(&world.state, &world.source(), Some(1328), None, None, true)
        .await
        .unwrap();
    assert_eq!(answer["available"], false);
    assert_eq!(world.arguments(), None);
}

#[tokio::test]
async fn an_older_build_is_installed_only_when_its_tag_is_named() {
    let world = World::new();
    let older = world.release(1326);
    world.latest(&older);
    let answer = update(&world.state, &world.source(), Some(1327), None, None, false)
        .await
        .unwrap();
    assert_eq!(answer["updated"], false);
    assert_eq!(world.arguments(), None);

    let answer = update(
        &world.state,
        &world.source(),
        Some(1327),
        None,
        Some("v0.1.0-1326"),
        false,
    )
    .await
    .unwrap();
    assert_eq!(answer["to"]["build"], 1326);
    assert!(world.arguments().is_some());
}

#[tokio::test]
async fn a_corrupted_archive_fails_before_anything_is_replaced() {
    let world = World::new();
    let release = world.release(1328);
    world.latest(&release);
    let name = format!(
        "releases/v0.1.0-1328/latch-v0.1.0-1328-{}.tar.gz",
        target().unwrap()
    );
    let mut archive = world.server.files.lock().unwrap()[&name].clone();
    let last = archive.len() - 1;
    archive[last] ^= 1;
    world.put(&name, archive);
    let failure = update(&world.state, &world.source(), Some(1327), None, None, false)
        .await
        .unwrap_err();
    assert_eq!(failure.code, "bad_download", "{}", failure.message);
    assert_eq!(world.arguments(), None);
}

#[tokio::test]
async fn an_unsigned_or_wrongly_signed_release_is_refused() {
    let world = World::new();
    let release = world.release(1328);
    world.latest(&release);
    // Signed with another key.
    let other = Key::new();
    let sums = world.server.files.lock().unwrap()["releases/v0.1.0-1328/SHA256SUMS"].clone();
    world.put("releases/v0.1.0-1328/SHA256SUMS.minisig", other.sign(&sums));
    let failure = update(&world.state, &world.source(), Some(1327), None, None, false)
        .await
        .unwrap_err();
    assert_eq!(
        (failure.code.as_str(), failure.exit),
        ("bad_signature", Exit::Denied)
    );
    // Not signed at all.
    world
        .server
        .files
        .lock()
        .unwrap()
        .remove("releases/v0.1.0-1328/SHA256SUMS.minisig");
    let failure = update(&world.state, &world.source(), Some(1327), None, None, false)
        .await
        .unwrap_err();
    assert_eq!(failure.code, "bad_signature");
    assert!(
        failure.message.contains("is not signed"),
        "{}",
        failure.message
    );
    // A latest.json someone else signed names nothing.
    world.put("latest.json.minisig", other.sign(b"{}"));
    let failure = update(&world.state, &world.source(), Some(1327), None, None, true)
        .await
        .unwrap_err();
    assert_eq!(failure.code, "bad_signature");
    assert_eq!(world.arguments(), None);
}

#[tokio::test]
async fn a_local_build_never_updates_itself() {
    let world = World::new();
    let release = world.release(1328);
    world.latest(&release);
    let failure = update(&world.state, &world.source(), None, None, None, false)
        .await
        .unwrap_err();
    assert_eq!(failure.code, "local_build");
    assert_eq!(*world.server.requests.lock().unwrap(), 0);
    let answer = update(&world.state, &world.source(), None, None, None, true)
        .await
        .unwrap();
    assert_eq!(answer["available"], false);
    assert_eq!(world.arguments(), None);
}

/// The key latch carries is the release key, DA695E5C4D2D36B9 as minisign
/// shows its id: the key number's bytes, last first.
#[test]
fn latch_carries_the_release_key() {
    let key = minisign::PublicKey::from_base64(RELEASE_KEY).unwrap();
    let id: String = key
        .keynum()
        .iter()
        .rev()
        .map(|b| format!("{b:02X}"))
        .collect();
    assert_eq!(id, "DA695E5C4D2D36B9");
    Source::releases();
}

#[test]
fn only_a_release_tag_names_a_release() {
    assert_eq!(
        Release::named("v0.1.0-1327").unwrap(),
        Release {
            tag: "v0.1.0-1327".into(),
            build: 1327
        }
    );
    for tag in [
        "0.1.0-1327",
        "v0.1.0",
        "v0.1-1327",
        "v0.1.0-main.4",
        "v0.1.0-",
        "v0.1.0-12a",
    ] {
        assert_eq!(Release::named(tag).unwrap_err().code, "bad_tag", "{tag}");
    }
}

#[test]
fn the_installed_engines_are_the_ones_kept_up_to_date() {
    let world = World::new();
    let state = &world.state.path;
    assert_eq!(engines(state), ["--no-browser"]);
    std::fs::create_dir_all(state.join("plugins/cft")).unwrap();
    assert_eq!(engines(state), ["--browser", "chrome"]);
    std::fs::create_dir_all(state.join("plugins/camoufox")).unwrap();
    assert_eq!(
        engines(state),
        ["--browser", "chrome", "--browser", "camoufox"]
    );
}

#[tokio::test]
async fn a_look_finds_a_newer_build_for_status_and_the_dashboard() {
    let world = World::new();
    let release = world.release(1328);
    world.latest(&release);
    let state = &world.state.path;
    assert_eq!(
        status_of(&Checked::load(state), Some(1327)),
        Status {
            available: false,
            build: None,
            checked_at: None
        }
    );
    look(&world.source(), state).await.unwrap();
    let checked = Checked::load(state);
    let status = status_of(&checked, Some(1327));
    assert!(status.available && status.build == Some(1328), "{status:?}");
    assert!(status.checked_at.is_some());
    assert_eq!(
        available_line(&checked, 1327).as_deref(),
        Some("Build 1328 is available. Run `latch update`.")
    );
    assert_eq!(available_line(&checked, 1328), None);
    assert!(!status_of(&checked, Some(1328)).available);
    // A local build is never behind.
    assert!(!status_of(&checked, None).available);
}

/// Looks are a day apart, whether the last one worked or not; one that
/// failed keeps what the last one that worked found.
#[tokio::test]
async fn a_failed_look_keeps_what_was_found_and_the_next_is_a_day_later() {
    let world = World::new();
    let release = world.release(1328);
    world.latest(&release);
    let state = &world.state.path;
    assert_eq!(Checked::load(state).due(), None);
    look(&world.source(), state).await.unwrap();
    let found = Checked::load(state);
    let day = SignedDuration::from_hours(24);
    assert_eq!(found.attempted_at, found.checked_at);
    assert_eq!(found.due(), Some(found.attempted_at.unwrap() + day));

    // Offline: nothing answers there.
    let offline = Source {
        url: "http://127.0.0.1:9/latch-cli".into(),
        ..world.source()
    };
    assert_eq!(
        look(&offline, state).await.unwrap_err().code,
        "releases_unreachable"
    );
    let checked = Checked::load(state);
    assert_eq!(checked.latest, Some(release));
    assert_eq!(checked.checked_at, found.checked_at);
    assert!(checked.attempted_at > found.attempted_at);
    assert_eq!(checked.due(), Some(checked.attempted_at.unwrap() + day));
    assert!(status_of(&checked, Some(1327)).available);
}

#[test]
fn a_terminal_hears_of_a_newer_build_once_a_day() {
    let now = Timestamp::now();
    let mut checked = Checked {
        latest: Some(Release::named("v0.1.0-1328").unwrap()),
        ..Checked::default()
    };
    assert_eq!(
        notice_line(&checked, 1327, now).as_deref(),
        Some("latch: build 1328 is available; run `latch update` to install it")
    );
    assert_eq!(notice_line(&checked, 1328, now), None);
    checked.noticed_at = Some(now - SignedDuration::from_hours(23));
    assert_eq!(notice_line(&checked, 1327, now), None);
    checked.noticed_at = Some(now - SignedDuration::from_hours(25));
    assert!(notice_line(&checked, 1327, now).is_some());
}

/// A bundle signed by anyone but Plow is refused before it is installed:
/// here, signed ad hoc, as a re-signed or rebuilt app would be.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn an_app_not_signed_by_plow_is_refused() {
    let world = World::new();
    let app = world.dir.path().join("Plow Latch.app");
    let macos = app.join("Contents/MacOS");
    std::fs::create_dir_all(&macos).unwrap();
    std::fs::copy("/usr/bin/true", macos.join("latch")).unwrap();
    std::fs::write(
        app.join("Contents/Info.plist"),
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>co.plow.latch.cli</string>
<key>CFBundleExecutable</key><string>latch</string>
</dict></plist>
"#,
    )
    .unwrap();
    let signed = std::process::Command::new("codesign")
        .args(["--force", "--sign", "-"])
        .arg(&app)
        .status()
        .unwrap();
    assert!(signed.success());
    let failure = APP_SIGNER.check(&app).await.unwrap_err();
    assert_eq!(failure.code, "bad_app_signature", "{}", failure.message);
    assert!(
        failure
            .message
            .contains("not co.plow.latch.cli by team 3559PD337Z"),
        "{}",
        failure.message
    );
}

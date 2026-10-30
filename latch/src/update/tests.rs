//! Updates from a local release server, signed with a key made for each
//! test; the release's install-binary.sh is a stand-in that writes down
//! what it was given. Nothing here reaches the real releases.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use super::*;
use crate::failure::Exit;

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
        // A stand-in installer: it writes down its arguments, one a line.
        let script = if cfg!(windows) {
            format!(
                "[IO.File]::WriteAllLines('{}', [string[]]$args)\n'installing'\n\
                 if (Test-Path '{}') {{ [Console]::Error.WriteLine('cannot replace latch'); exit 1 }}\nexit 0\n",
                self.installed().display(),
                self.failing().display()
            )
        } else {
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\necho installing\n\
                 [ -e '{}' ] && {{ echo 'cannot replace latch' >&2; exit 1; }}\nexit 0\n",
                self.installed().display(),
                self.failing().display()
            )
        };
        std::fs::write(folder.join(INSTALLER), script).unwrap();
        let latch = if cfg!(windows) { "latch.exe" } else { "latch" };
        std::fs::write(folder.join(latch), format!("build {build}\n")).unwrap();
        let archive = made.join(format!("{name}.{ARCHIVE}"));
        // On Windows, a zip (`-a` picks the format by its name).
        let tar = std::process::Command::new(tar())
            .arg(if cfg!(windows) { "-acf" } else { "-czf" })
            .arg(&archive)
            .arg("-C")
            .arg(&made)
            .arg(&name)
            .status()
            .unwrap();
        assert!(tar.success());
        let archive = std::fs::read(archive).unwrap();
        let sums = format!(
            "{}  {name}.{ARCHIVE}\n{}  install.sh\n",
            to_hex(&Sha256::digest(&archive)),
            "0".repeat(64)
        );
        let folder = format!("releases/{}", release.tag);
        self.put(&format!("{folder}/{name}.{ARCHIVE}"), archive);
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

    /// While this file exists, the stand-in installer fails.
    fn failing(&self) -> PathBuf {
        self.dir.path().join("fail-install")
    }

    /// trace.log, its line ends as Unix writes them (PowerShell's are CRLF).
    fn trace_log(&self) -> String {
        std::fs::read_to_string(self.state.path.join(crate::trace::FILE))
            .unwrap_or_default()
            .replace("\r\n", "\n")
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
    let latch = Path::new(&format!("latch-v0.1.0-1328-{}", target().unwrap()))
        .join(if cfg!(windows) { "latch.exe" } else { "latch" });
    assert!(Path::new(&arguments[6]).ends_with(&latch), "{arguments:?}");
}

/// On Linux, latch is installed where this one runs from; on macOS the
/// installer replaces the app every link leads into.
/// The installer stops the host: one `serve -d` started runs again, and
/// one that ran in a terminal is said to be stopped, with what starts it.
#[test]
fn an_update_says_what_became_of_the_host() {
    assert_eq!(host_after(false, false, false), None);
    assert_eq!(
        host_after(true, false, true),
        Some(json!({"running": true}))
    );
    assert_eq!(
        host_after(true, true, false),
        Some(json!({"running": false, "next": "latch serve"}))
    );
    assert_eq!(
        host_after(true, false, false),
        Some(json!({"running": false, "next": "latch serve --foreground"}))
    );
}

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
        "releases/v0.1.0-1328/latch-v0.1.0-1328-{}.{ARCHIVE}",
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

/// What tar and the installer print goes to trace.log, never this
/// command's stdout or stderr, whether they succeed or fail.
#[tokio::test]
async fn what_tar_and_the_installer_print_goes_to_trace_log() {
    let world = World::new();
    let release = world.release(1328);
    world.latest(&release);
    std::fs::write(world.failing(), "").unwrap();
    let failure = update(&world.state, &world.source(), Some(1327), None, None, false)
        .await
        .unwrap_err();
    assert_eq!(failure.code, "install_failed", "{}", failure.message);
    assert!(failure.message.contains("trace.log"), "{}", failure.message);
    let traced = world.trace_log();
    assert!(
        traced.contains("installing\ncannot replace latch\n"),
        "{traced}"
    );

    std::fs::remove_file(world.failing()).unwrap();
    update(&world.state, &world.source(), Some(1327), None, None, false)
        .await
        .unwrap();
    assert!(
        world.trace_log().ends_with("installing\n"),
        "{}",
        world.trace_log()
    );

    // An archive its sums list, but no archive: tar's complaint goes there too.
    let name = format!(
        "releases/v0.1.0-1329/latch-v0.1.0-1329-{}.{ARCHIVE}",
        target().unwrap()
    );
    let release = world.release(1329);
    world.latest(&release);
    let garbage = b"not a tarball".to_vec();
    let sums = format!(
        "{}  {}\n",
        to_hex(&Sha256::digest(&garbage)),
        name.rsplit('/').next().unwrap()
    );
    world.put(&name, garbage);
    world.put(
        "releases/v0.1.0-1329/SHA256SUMS.minisig",
        world.key.sign(sums.as_bytes()),
    );
    world.put("releases/v0.1.0-1329/SHA256SUMS", sums.into_bytes());
    let before = world.trace_log().len();
    let failure = update(&world.state, &world.source(), Some(1328), None, None, false)
        .await
        .unwrap_err();
    assert_eq!(failure.code, "bad_download", "{}", failure.message);
    assert!(world.trace_log().len() > before, "tar said nothing");
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

/// An update cut off after the installer replaced latch, before it fetched
/// the new pins, leaves this build current with its helpers missing: the
/// next plain update installs this build again, and one with every helper
/// in place does nothing.
#[tokio::test]
async fn a_cut_off_update_is_finished_by_the_next_one() {
    let world = World::new();
    let release = world.release(1328);
    world.latest(&release);
    let state = &world.state.path;
    // An older Camoufox and gog, from the latch the cut-off update replaced.
    std::fs::create_dir_all(state.join("plugins/camoufox/0.0.1")).unwrap();
    std::fs::create_dir_all(state.join("plugins/gog/0.0.1")).unwrap();
    let answer = update(&world.state, &world.source(), Some(1328), None, None, false)
        .await
        .unwrap();
    // Windows' installer puts in no helpers (no gog, and Camoufox has no
    // Windows build): an up-to-date install stays as it is.
    if cfg!(windows) {
        assert_eq!(answer["updated"], false, "{answer}");
        assert!(world.arguments().is_none(), "the installer never ran");
        return;
    }
    assert_eq!(answer["updated"], true, "{answer}");
    assert_eq!(answer["to"]["build"], 1328);
    assert_eq!(answer["repaired"], json!(["gog", "camoufox"]));
    let arguments = world.arguments().expect("install-binary.sh ran");
    assert_eq!(
        arguments[2..5],
        ["--non-interactive", "--browser", "camoufox"]
    );

    let complete = World::new();
    complete.latest(&complete.release(1328));
    let gog = complete
        .state
        .path
        .join("plugins/gog")
        .join(&crate::google::PIN.version);
    std::fs::create_dir_all(&gog).unwrap();
    std::fs::write(gog.join("gog"), "").unwrap();
    let answer = update(
        &complete.state,
        &complete.source(),
        Some(1328),
        None,
        None,
        false,
    )
    .await
    .unwrap();
    assert_eq!(answer["updated"], false, "{answer}");
    assert_eq!(complete.arguments(), None);
}

#[test]
fn the_installed_engines_are_the_ones_kept_up_to_date() {
    let world = World::new();
    let state = &world.state.path;
    assert_eq!(helpers(state), ["--no-browser"]);
    std::fs::create_dir_all(state.join("plugins/cft")).unwrap();
    assert_eq!(helpers(state), ["--browser", "chrome"]);
    std::fs::create_dir_all(state.join("plugins/camoufox")).unwrap();
    assert_eq!(
        helpers(state),
        ["--browser", "chrome", "--browser", "camoufox"]
    );
}

/// A machine installed with `--client-only` is updated the same way: latch
/// alone, whatever else is in its state folder.
#[tokio::test]
async fn a_client_only_install_is_updated_client_only() {
    let world = World::new();
    let release = world.release(1328);
    world.latest(&release);
    let state = &world.state.path;
    std::fs::create_dir_all(state.join("plugins/cft")).unwrap();
    std::fs::write(state.join("plugins/client-only"), "").unwrap();
    update(&world.state, &world.source(), Some(1327), None, None, false)
        .await
        .unwrap();
    let arguments = world.arguments().expect("install-binary.sh ran");
    assert_eq!(arguments[2..4], ["--non-interactive", "--client-only"]);
    assert_eq!(arguments.len(), 5, "{arguments:?}");
}

/// An agent updates its own computer, where latch is installed alone; on a
/// host it gets `owner_only`. The owner's login updates either.
#[tokio::test]
async fn an_agent_updates_only_its_own_client_only_computer() {
    let login = |world: &World, kind: &str| {
        let peer = crate::plow::Peer {
            key: "a".repeat(64),
            kind: kind.into(),
            name: "research-agent".into(),
            relay_url: None,
            last_seen_at: None,
            online: None,
            this_machine: false,
        };
        let api_url = "http://127.0.0.1:9".to_string();
        let config = crate::state::Config { api_url, peer };
        world.state.create().unwrap();
        world.state.save_config(&config).unwrap();
    };
    let client_only = |world: &World| {
        std::fs::create_dir_all(world.state.path.join("plugins")).unwrap();
        std::fs::write(world.state.path.join("plugins/client-only"), "").unwrap();
    };

    let host = World::new();
    login(&host, "agent");
    let failure = may_update(&host.state, &[]).unwrap_err();
    assert_eq!(failure.code, "owner_only", "{failure:?}");
    assert!(failure.message.contains("--client-only"), "{failure:?}");
    login(&host, "device");
    may_update(&host.state, &[]).unwrap();

    let own = World::new();
    own.latest(&own.release(1328));
    login(&own, "agent");
    client_only(&own);
    may_update(&own.state, &[]).unwrap();
    let answer = update(&own.state, &own.source(), Some(1327), None, None, false)
        .await
        .unwrap();
    assert_eq!(answer["updated"], true, "{answer}");
    let arguments = own.arguments().expect("install-binary.sh ran");
    assert_eq!(arguments[2..4], ["--non-interactive", "--client-only"]);
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

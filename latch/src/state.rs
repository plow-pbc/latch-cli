//! The state directory of one latch instance (`--state-dir`, default
//! `~/.latch`). Everything in it is readable by this user only. A latch
//! signed for the Keychain keeps the secrets marked * there instead of
//! in files (see `secrets.rs`):
//!
//! - `key`*: this peer's ed25519 secret key, as hex. It never leaves the machine.
//! - `key.lock`: held by whatever puts that key on the network (see `KeyUse`).
//! - `plow-token`*: the Plow credential `login --token-file` was given (a
//!   host's owner token). An agent's own credential is not copied here; see
//!   `credential.rs`.
//! - `config.json`: the Plow API URL and this peer's record as Plow returned it.
//! - `plow-url`: the Plow a saved token belongs to, written with it, so a
//!   login that fails after the token arrived is retried against that Plow.
//! - `audit.sealed.ndjson`, on a host: its audit log, each record sealed
//!   (see `audit.rs`); `audit.keys`*: the keys that seal it; `audit.lock`:
//!   held by what writes it. A plaintext `audit.ndjson` from an older latch
//!   is never read, and every command says it can be deleted.
//! - `serve.log`, on a host: what a managed host logs (`latch logs`);
//!   `serve.log.1`, the one before, once it grew past 10 MB.
//! - `serve.plist` (macOS) and `launch.lock`, on a host: the managed host's
//!   service, and the lock that starts and stops it one at a time (see
//!   `managed.rs`).
//! - `settings.json`: this machine's host settings (see `settings.rs`);
//!   `keep-awake.json`, on a host: what it holds (see `keep_awake.rs`).
//! - `vault.json` and `vault.key`*, on a host: the vault and its custody key
//!   (see `vault/store.rs`).
//! - `browser/`, on a host: open sessions' profiles, the model's settings
//!   and each session's journal.
//! - `plugins/`, on a host: the pinned engines, the Google CLI and the
//!   Messages reader, a folder per version; a starting host removes the
//!   versions it does not run.
//! - `leftovers/`: what an older install left in an unmarked `~/.latch`,
//!   moved there, a folder per run (see `check_format`).
//! - `latch.json`: the folder's format (see `FORMAT`), which every command
//!   checks before it reads or writes anything else.

use std::fs;
use std::io::{ErrorKind, Write};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

use iroh::{PublicKey, SecretKey};
use serde::{Deserialize, Serialize};

use crate::failure::{Failure, code};
use crate::plow::Peer;
use crate::secrets::{Secret, Secrets};

/// What `login` remembers for later commands.
#[derive(Serialize, Deserialize, Debug)]
pub struct Config {
    pub api_url: String,
    pub peer: Peer,
}

/// The layout of the files in a state folder. A latch reads only a folder
/// of exactly its own format: an older latch would misread a newer folder (or
/// silently drop what it does not know when it saves), and no newer latch
/// converts older folders. Raise it with a change an older latch would misread.
const FORMAT: u32 = 2;

/// `latch.json`: the folder's format, and which latch first marked it.
#[derive(Serialize, Deserialize)]
struct Marker {
    format: u32,
    written_by: String,
}

/// Who puts this folder's key on the network now (`StateDir::use_key`).
pub enum KeyUse {
    /// No host does: dial with the key, holding it until the endpoint closes.
    Direct(Box<HeldKey>),
    /// A host runs on this folder, and only it uses the key: go through it.
    Host,
}

/// The folder's secret key and its `key.lock`, held while an endpoint
/// bound with it is open; the only way to bind one with it.
pub struct HeldKey {
    key: SecretKey,
    _lock: fs::File,
}

impl HeldKey {
    /// An endpoint builder with this key.
    pub fn endpoint(&self) -> iroh::endpoint::Builder {
        crate::dial::endpoint().secret_key(self.key.clone())
    }
}

pub struct StateDir {
    pub path: PathBuf,
}

impl StateDir {
    /// `--state-dir` if given, else `~/.latch`.
    pub fn new(flag: Option<PathBuf>) -> Result<StateDir, Failure> {
        let path = match flag {
            Some(path) => path,
            None => match std::env::home_dir() {
                Some(home) => home.join(".latch"),
                None => {
                    let message = "no home directory; pass --state-dir".to_string();
                    return Err(Failure::new(code::no_state_dir, message));
                }
            },
        };
        Ok(StateDir { path })
    }

    /// Creates the directory (owner-only) if it does not exist yet.
    pub fn create(&self) -> Result<(), Failure> {
        self.check_format()?;
        fs::create_dir_all(&self.path).map_err(|err| self.io_failure(&self.path, err))?;
        let owner_only = fs::Permissions::from_mode(0o700);
        fs::set_permissions(&self.path, owner_only)
            .map_err(|err| self.io_failure(&self.path, err))?;
        if !self.path.join("latch.json").exists() {
            self.save_json(
                "latch.json",
                &Marker {
                    format: FORMAT,
                    written_by: crate::cli::version().to_string(),
                },
            )?;
        }
        Ok(())
    }

    /// Refuses unmarked peer state and folders of another format.
    /// Missing, empty or plugin-only folders are initialized only by `create`;
    /// an empty lock file a command left there holds no state either, nor
    /// does the Finder's `.DS_Store`, nor `leftovers/`. In the default
    /// folder, anything else without a marker is what an older install left:
    /// it is moved into `leftovers/` (`move_aside`), never read or deleted,
    /// and the command goes on.
    pub fn check_format(&self) -> Result<(), Failure> {
        let default = StateDir::new(crate::cli::BUILD_STATE_DIR.map(PathBuf::from))
            .is_ok_and(|default| default.path == self.path);
        self.check_format_moving(default)
    }

    fn check_format_moving(&self, move_leftovers: bool) -> Result<(), Failure> {
        let path = self.path.join("latch.json");
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == ErrorKind::NotFound => {
                let found = match self.leftovers() {
                    Ok(found) if found.is_empty() => return Ok(()),
                    Ok(found) => found,
                    Err(err) if err.kind() == ErrorKind::NotFound => return Ok(()),
                    Err(err) => return Err(self.io_failure(&self.path, err)),
                };
                if move_leftovers {
                    return self.move_aside(&found);
                }
                let aside = shell_word(&self.aside());
                let moved: Vec<String> = found
                    .iter()
                    .map(|name| shell_word(&self.path.join(name)))
                    .collect();
                let command = format!("mkdir -p -m 700 {aside} && mv {} {aside}/", moved.join(" "));
                let mut failure = Failure::new(
                    code::state_format,
                    format!(
                        "{} has no latch.json format marker and holds what latch did not put there ({}). \
                         Nothing was read or changed: move them aside with `{command}`, or pass another --state-dir",
                        self.path.display(),
                        found.join(", "),
                    ),
                );
                failure.next = Some(command.into());
                failure.details = serde_json::json!({ "found": found });
                return Err(failure);
            }
            Err(err) => return Err(self.io_failure(&path, err)),
        };
        let marker: Marker = serde_json::from_str(&text).map_err(|err| {
            let message = format!("{} is not valid: {err}", path.display());
            Failure::new(code::bad_state, message)
        })?;
        let fix = match marker.format.cmp(&FORMAT) {
            std::cmp::Ordering::Equal => return Ok(()),
            std::cmp::Ordering::Greater => "update this latch",
            std::cmp::Ordering::Less => "use the latch that wrote it",
        };
        let message = format!(
            "{} is in state format {}, set up by latch {}; this latch {} reads only format {FORMAT}: \
             {fix}, or pass another --state-dir. Nothing was read or changed",
            self.path.display(),
            marker.format,
            marker.written_by,
            crate::cli::version(),
        );
        Err(Failure::new(code::state_format, message))
    }

    /// What an unmarked folder holds that latch neither made nor ignores:
    /// not the plugins or leftovers folder, an empty lock file or a
    /// `.DS_Store`.
    fn leftovers(&self) -> std::io::Result<Vec<String>> {
        let mut found = Vec::new();
        for entry in fs::read_dir(&self.path)? {
            let entry = entry?;
            let path = entry.path();
            let ours = (["plugins", "leftovers"]
                .map(std::ffi::OsStr::new)
                .contains(&&*entry.file_name())
                && path.is_dir())
                || entry.file_name() == ".DS_Store"
                || (path.extension().is_some_and(|ext| ext == "lock")
                    && fs::symlink_metadata(&path)
                        .is_ok_and(|meta| meta.is_file() && meta.len() == 0));
            if !ours {
                found.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
        found.sort();
        Ok(found)
    }

    /// A folder in `leftovers/` no one has used yet: `leftovers/DATE`, then
    /// `DATE-2` and on. Inside the state folder, so what it keeps is as
    /// hidden from workers as the rest of it.
    fn aside(&self) -> PathBuf {
        let first = jiff::Zoned::now().date().to_string();
        std::iter::once(first.clone())
            .chain((2..).map(|n| format!("{first}-{n}")))
            .map(|name| self.path.join("leftovers").join(name))
            .find(|path| fs::symlink_metadata(path).is_err())
            .expect("a name not taken")
    }

    /// Moves `found` into a new folder in `leftovers/` (owner-only), and says
    /// so on stderr.
    fn move_aside(&self, found: &[String]) -> Result<(), Failure> {
        let aside = self.aside();
        fs::DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(self.path.join("leftovers"))
            .and_then(|()| fs::DirBuilder::new().mode(0o700).create(&aside))
            .map_err(|err| self.io_failure(&aside, err))?;
        for name in found {
            fs::rename(self.path.join(name), aside.join(name))
                .map_err(|err| self.io_failure(&self.path.join(name), err))?;
        }
        eprintln!(
            "latch: {} held what an older install left ({}); moved them to {}",
            self.path.display(),
            found.join(", "),
            aside.display()
        );
        Ok(())
    }

    /// Where this folder keeps its secrets (see `secrets.rs`).
    pub fn secrets(&self) -> Result<Secrets, Failure> {
        Secrets::open(&self.path)
    }

    /// Loads this peer's key, generating and saving it on first use.
    /// Returns its public half and whether it was just created.
    pub fn load_or_create_key(&self) -> Result<(PublicKey, bool), Failure> {
        let key = SecretKey::generate();
        let hex = to_hex(&key.to_bytes());
        if self.secrets()?.create(Secret::Key, hex.as_bytes())? {
            Ok((key.public(), true))
        } else {
            Ok((self.public_key()?, false))
        }
    }

    /// This peer's public key: who it is to Plow and to hosts.
    pub fn public_key(&self) -> Result<PublicKey, Failure> {
        Ok(self.load_key()?.public())
    }

    /// For a command about to dial another host: the key, held with any
    /// other such command's, or `Host` when a host runs on this folder.
    /// The relay delivers to the newest endpoint of a key, so an endpoint
    /// bound beside the host's would take the host's callers from it. A
    /// host from another latch takes no key lock, but holds `serve.lock`;
    /// so does one waiting in `own_key` for this command to finish.
    pub fn use_key(&self) -> Result<KeyUse, Failure> {
        let lock = self.key_lock()?;
        match lock.try_lock_shared() {
            Ok(()) => {}
            Err(fs::TryLockError::WouldBlock) => return Ok(KeyUse::Host),
            Err(fs::TryLockError::Error(err)) => return Err(self.key_lock_failure(err)),
        }
        if crate::lifecycle::now(&self.path).running {
            return Ok(KeyUse::Host);
        }
        Ok(KeyUse::Direct(Box::new(HeldKey {
            key: self.load_key()?,
            _lock: lock,
        })))
    }

    /// For `latch serve`: the key, held alone for as long as it serves.
    /// While commands that dial directly hold it, this blocks until they
    /// let go, calling `waiting` first. A folder not logged in has none.
    pub fn own_key(&self, waiting: impl FnOnce()) -> Result<HeldKey, Failure> {
        self.require_login()?;
        let lock = self.key_lock()?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(fs::TryLockError::WouldBlock) => {
                waiting();
                lock.lock().map_err(|err| self.key_lock_failure(err))?;
            }
            Err(fs::TryLockError::Error(err)) => return Err(self.key_lock_failure(err)),
        }
        Ok(HeldKey {
            key: self.load_key()?,
            _lock: lock,
        })
    }

    fn key_lock(&self) -> Result<fs::File, Failure> {
        use std::os::unix::fs::OpenOptionsExt;
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(self.path.join("key.lock"))
            .map_err(|err| self.key_lock_failure(err))
    }

    fn key_lock_failure(&self, err: std::io::Error) -> Failure {
        let path = self.path.join("key.lock");
        Failure::new(code::state_io, format!("{}: {err}", path.display()))
    }

    /// The secret key, which only a `HeldKey` hands on.
    fn load_key(&self) -> Result<SecretKey, Failure> {
        let Some(bytes) = self.secrets()?.read(Secret::Key)? else {
            let message = format!(
                "{} has no key; run `latch login` first",
                self.path.display()
            );
            return Err(Failure::new(code::state_io, message));
        };
        // `SecretKey` parses itself from the hex we wrote.
        let parsed = String::from_utf8(bytes)
            .ok()
            .and_then(|text| text.trim().parse().ok());
        parsed.ok_or_else(|| {
            let message = format!("{}: its key is not a valid key", self.path.display());
            Failure::new(code::bad_state, message)
        })
    }

    fn save_json(&self, name: &str, value: &impl Serialize) -> Result<(), Failure> {
        let path = self.path.join(name);
        let text = serde_json::to_string_pretty(value).expect("state serializes") + "\n";
        write_private(&path, text.as_bytes()).map_err(|err| self.io_failure(&path, err))
    }

    /// The token saved by `login --token-file`, if there is one.
    pub fn load_token(&self) -> Result<Option<String>, Failure> {
        let Some(bytes) = self.secrets()?.read(Secret::PlowToken)? else {
            return Ok(None);
        };
        let text = String::from_utf8(bytes).map_err(|_| {
            let message = format!("{}: its Plow token is not text", self.path.display());
            Failure::new(code::bad_state, message)
        })?;
        Ok(Some(text.trim().to_string()))
    }

    /// Saves a Plow token and the Plow it belongs to.
    pub fn save_token(&self, token: &str, api_url: &str) -> Result<(), Failure> {
        self.secrets()?.write(Secret::PlowToken, token.as_bytes())?;
        let path = self.path.join("plow-url");
        write_private(&path, format!("{api_url}\n").as_bytes())
            .map_err(|err| self.io_failure(&path, err))
    }

    /// The Plow the saved token belongs to, if one was recorded with it.
    pub fn load_plow_url(&self) -> Result<Option<String>, Failure> {
        let path = self.path.join("plow-url");
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(self.read(&path)?.trim().to_string()))
    }

    /// The saved config, or `None` before the first successful `login`.
    pub fn load_config(&self) -> Result<Option<Config>, Failure> {
        let path = self.path.join("config.json");
        if !path.exists() {
            return Ok(None);
        }
        let text = self.read(&path)?;
        let config = serde_json::from_str(&text).map_err(|err| {
            let message = format!("{} is not valid: {err}", path.display());
            Failure::new(code::bad_state, message)
        })?;
        Ok(Some(config))
    }

    pub fn save_config(&self, config: &Config) -> Result<(), Failure> {
        let path = self.path.join("config.json");
        // Serializing our own plain struct cannot fail.
        let text = serde_json::to_string_pretty(config).expect("config serializes") + "\n";
        write_private(&path, text.as_bytes()).map_err(|err| self.io_failure(&path, err))
    }

    /// Like `load_config`, for commands that need a completed `login`. Reads
    /// no secret: a call to this machine's own host over its socket needs
    /// none.
    pub fn require_config(&self) -> Result<Config, Failure> {
        self.load_config()?.ok_or_else(|| {
            let message = format!(
                "{} is not logged in; run `latch login` first",
                self.path.display()
            );
            Failure::new(code::not_logged_in, message)
        })
    }

    /// `require_config`, and checks that the key on disk is the one Plow
    /// registered, so a replaced or restored key is caught here rather than
    /// at a host.
    pub fn require_login(&self) -> Result<Config, Failure> {
        let config = self.require_config()?;
        if self.public_key()?.to_string() != config.peer.key {
            let message = format!(
                "{}: the key does not match the key this instance registered with Plow",
                self.path.display()
            );
            return Err(Failure::new(code::key_mismatch, message));
        }
        Ok(config)
    }

    fn read(&self, path: &Path) -> Result<String, Failure> {
        fs::read_to_string(path).map_err(|err| self.io_failure(path, err))
    }

    fn io_failure(&self, path: &Path, err: std::io::Error) -> Failure {
        Failure::new(code::state_io, format!("{}: {err}", path.display()))
    }
}

/// A path as one word for the shell.
fn shell_word(path: &Path) -> String {
    crate::login::shell_quoted(&path.to_string_lossy())
}

/// Writes a file only its owner can read, atomically and durably: into a
/// temporary file of this write's own beside it (0600), synced, renamed
/// over it, and the folder synced, so the new file is on disk before
/// anything that relies on it (audit records sealed under a new key) is.
/// Concurrent writers never share a temporary file. Skips the write when
/// the file already holds exactly these bytes, so a repeated `login`
/// changes nothing.
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if fs::read(path).is_ok_and(|current| current == bytes) {
        return Ok(());
    }
    let dir = path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut name = path.file_name().unwrap_or_default().to_owned();
    name.push(".");
    let mut temp = tempfile::Builder::new()
        .prefix(&name)
        .suffix(".tmp")
        .tempfile_in(dir)?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|err| err.error)?;
    fs::File::open(dir)?.sync_all()
}

/// Bytes as lowercase hex, two characters per byte.
pub fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test state lives under the repo's gitignored `.dev/`, like every dev run.
    fn test_dir() -> tempfile::TempDir {
        let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.dev/tests");
        fs::create_dir_all(&parent).unwrap();
        tempfile::tempdir_in(parent).unwrap()
    }

    #[test]
    fn a_host_takes_the_key_alone_once_the_commands_dialing_with_it_let_go() {
        let dir = test_dir();
        let state = StateDir {
            path: dir.path().to_path_buf(),
        };
        let (key, _) = state.load_or_create_key().unwrap();
        let peer = crate::plow::Peer {
            key: key.to_string(),
            kind: "device".into(),
            name: "studio".into(),
            relay_url: None,
            last_seen_at: None,
            online: None,
            this_machine: false,
        };
        let api_url = "http://127.0.0.1:9".to_string();
        state.save_config(&Config { api_url, peer }).unwrap();
        let direct = |state: &StateDir| match state.use_key().unwrap() {
            KeyUse::Direct(key) => key,
            KeyUse::Host => panic!("no host holds the key"),
        };
        // Commands share it.
        let (first, second) = (direct(&state), direct(&state));
        let (waited, waiting) = std::sync::mpsc::channel();
        let path = state.path.clone();
        let host =
            std::thread::spawn(move || StateDir { path }.own_key(move || waited.send(()).unwrap()));
        waiting.recv().unwrap();
        drop(first);
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(
            !host.is_finished(),
            "the host took a key a command still held"
        );
        drop(second);
        let held = host.join().unwrap().unwrap();
        // While the host holds it, a command goes through the host.
        assert!(matches!(state.use_key().unwrap(), KeyUse::Host));
        drop(held);
        direct(&state);
    }

    #[test]
    fn a_folder_is_marked_once_and_one_of_another_format_is_refused() {
        let dir = test_dir();
        let state = StateDir::new(Some(dir.path().join("state"))).unwrap();
        let marker = state.path.join("latch.json");
        // Nothing to read yet, and nothing made: `create` marks the folder.
        state.check_format().unwrap();
        assert!(!state.path.exists());
        state.create().unwrap();
        let written = fs::read_to_string(&marker).unwrap();
        assert!(
            written.contains(&format!("\"format\": {FORMAT}")),
            "{written}"
        );
        state.check_format().unwrap();
        state.create().unwrap();
        assert_eq!(fs::read_to_string(&marker).unwrap(), written);

        fs::remove_file(&marker).unwrap();
        fs::create_dir(state.path.join("plugins")).unwrap();
        fs::write(state.path.join("key"), "private state").unwrap();
        assert_eq!(state.check_format().unwrap_err().code, "state_format");
        assert_eq!(state.create().unwrap_err().code, "state_format");
        assert!(!marker.exists());

        for (format, fix) in [
            (FORMAT + 1, "update this latch"),
            (FORMAT - 1, "use the latch that wrote it"),
        ] {
            let other = format!(r#"{{"format": {format}, "written_by": "9.9.9"}}"#);
            fs::write(&marker, &other).unwrap();
            let refused = state.check_format().unwrap_err();
            assert_eq!(refused.code, "state_format");
            assert!(refused.message.contains(fix), "{}", refused.message);
            assert!(refused.message.contains("9.9.9"), "{}", refused.message);
            assert_eq!(fs::read_to_string(&marker).unwrap(), other);
        }
    }

    #[test]
    fn an_unmarked_folder_with_only_empty_lock_files_is_still_a_fresh_one() {
        let dir = test_dir();
        let state = StateDir::new(Some(dir.path().join("state"))).unwrap();
        fs::create_dir_all(state.path.join("plugins")).unwrap();
        fs::write(state.path.join("launch.lock"), "").unwrap();
        fs::write(state.path.join("audit.lock"), "").unwrap();
        state.check_format().unwrap();

        // A lock file with something in it, or a folder named like one, is
        // state this latch didn't mark.
        let held = state.path.join("serve.lock");
        fs::write(&held, "latch serve (pid 1)").unwrap();
        assert_eq!(state.check_format().unwrap_err().code, "state_format");
        assert_eq!(state.create().unwrap_err().code, "state_format");
        fs::remove_file(&held).unwrap();
        let folder = state.path.join("held.lock");
        fs::create_dir(&folder).unwrap();
        assert_eq!(state.check_format().unwrap_err().code, "state_format");
        fs::remove_dir(&folder).unwrap();

        state.create().unwrap();
        assert!(state.path.join("latch.json").is_file());
    }

    /// What an older install left in the default folder (another app's
    /// files beside the installer's plugins) is moved into its `leftovers/`,
    /// whole, and the folder is then a fresh one; anywhere else it is refused, with the
    /// command that moves it. The Finder's `.DS_Store` is never in the way.
    #[test]
    fn leftovers_in_the_default_folder_are_moved_aside_and_elsewhere_refused() {
        let dir = test_dir();
        let state = StateDir::new(Some(dir.path().join(".latch"))).unwrap();
        let leave = |state: &StateDir| {
            fs::create_dir_all(state.path.join("plugins/gog")).unwrap();
            fs::write(state.path.join("dev.json"), "{\"mcpServers\": {}}").unwrap();
            fs::write(state.path.join("mba.json"), "{\"mcpServers\": {}}").unwrap();
            fs::write(state.path.join(".DS_Store"), "finder").unwrap();
        };
        leave(&state);

        let refused = state.check_format_moving(false).unwrap_err();
        assert_eq!(refused.code, "state_format");
        assert_eq!(
            refused.details["found"],
            serde_json::json!(["dev.json", "mba.json"])
        );
        let command = refused.next.as_deref().unwrap();
        assert!(refused.message.contains(command), "{}", refused.message);
        assert!(state.path.join("dev.json").exists(), "nothing moved");
        // The command it gives moves them, and the folder is a fresh one.
        let ran = std::process::Command::new("sh")
            .args(["-c", command])
            .status()
            .unwrap();
        assert!(ran.success(), "{command}");
        state.check_format_moving(false).unwrap();
        fs::remove_dir_all(&state.path).unwrap();

        leave(&state);
        state.check_format_moving(true).unwrap();
        let aside = state
            .path
            .join(format!("leftovers/{}", jiff::Zoned::now().date()));
        // Where workers can't read them, as with the rest of the folder.
        assert!(
            crate::sandbox::never_readable(dir.path())
                .iter()
                .any(|hidden| aside.starts_with(hidden)),
            "{}",
            aside.display()
        );
        let mut moved: Vec<_> = fs::read_dir(&aside)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        moved.sort();
        assert_eq!(moved, ["dev.json", "mba.json"]);
        assert_eq!(
            fs::read_to_string(aside.join("dev.json")).unwrap(),
            "{\"mcpServers\": {}}"
        );
        let mode = fs::metadata(&aside).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        assert!(state.path.join("plugins/gog").is_dir());
        assert!(state.path.join(".DS_Store").is_file());
        state.create().unwrap();
        assert!(state.path.join("latch.json").is_file());

        // Leftovers found again go beside the first ones, never over them.
        fs::remove_file(state.path.join("latch.json")).unwrap();
        fs::write(state.path.join("dev.json"), "again").unwrap();
        state.check_format_moving(true).unwrap();
        let second = aside.with_file_name(format!(
            "{}-2",
            aside.file_name().unwrap().to_string_lossy()
        ));
        assert_eq!(
            fs::read_to_string(second.join("dev.json")).unwrap(),
            "again"
        );

        // A `.DS_Store` alone is no leftover: nothing moved, nothing refused.
        let finder = StateDir::new(Some(dir.path().join("finder"))).unwrap();
        fs::create_dir(&finder.path).unwrap();
        fs::write(finder.path.join(".DS_Store"), "finder").unwrap();
        finder.check_format_moving(false).unwrap();
    }

    #[test]
    fn two_writers_at_once_each_leave_a_whole_file() {
        let dir = test_dir();
        let path = dir.path().join("config.json");
        // Big enough that one writer's bytes can land amid the other's.
        let contents = [vec![b'a'; 1 << 20], vec![b'b'; 1 << 20]];
        std::thread::scope(|scope| {
            for bytes in &contents {
                let path = &path;
                scope.spawn(move || {
                    for _ in 0..50 {
                        write_private(path, bytes).unwrap();
                        let _ = fs::remove_file(path);
                    }
                });
            }
            scope.spawn(|| {
                for _ in 0..500 {
                    if let Ok(read) = fs::read(&path) {
                        assert!(contents.contains(&read), "a mixed or partial file");
                    }
                }
            });
        });
        let names: Vec<_> = fs::read_dir(dir.path()).unwrap().collect();
        assert!(names.len() <= 1, "a temporary file was left: {names:?}");
    }

    #[test]
    fn key_is_created_once_and_private() {
        let dir = test_dir();
        let state = StateDir::new(Some(dir.path().join("state"))).unwrap();
        state.create().unwrap();

        let (first, created) = state.load_or_create_key().unwrap();
        assert!(created);
        let (again, created_again) = state.load_or_create_key().unwrap();
        assert!(!created_again);
        assert_eq!(first, again);

        let mode = fs::metadata(state.path.join("key"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let dir_mode = fs::metadata(&state.path).unwrap().permissions().mode();
        assert_eq!(dir_mode & 0o777, 0o700);
    }
}

//! `latch logout`: once any host on this state folder has stopped, an
//! owner's machine is taken out of the Plow account and its login revoked
//! there; then its Plow token is deleted (from the Keychain on a signed
//! macOS latch and as a file on any other), and with it `config.json`, the
//! record that this folder is logged in. Everything else stays: this
//! machine's key, the vault and audit log and their keys, rules, settings
//! and plugins. `latch login` then registers the same key, so under the
//! same name (the hostname, or the `--name` given again) an agent finds the
//! machine it knew. `--purge` deletes
//! every secret, and then the state folder itself. If Plow cannot be
//! reached or refuses, the local delete goes ahead and the output says the
//! machine is still in the account, for `latch devices remove` from
//! another of the owner's machines.
//!
//! Before it changes anything, logout says what it will delete and what it
//! keeps (`Summary`), and a person at a terminal confirms it: y for a
//! logout, typing `delete` for `--purge`. With no one at a terminal it
//! refuses, with that summary, unless given `--yes`. `--dry-run` only says.
//!
//! A folder deleted by hand leaves its Keychain items behind: logout on a
//! missing folder still takes the machine out of Plow, if its token is
//! there, and deletes them.
//!
//! The folder is resolved once, and must be a latch state folder, not a
//! link to one. From the stop on, logout holds `serve.lock` as a host
//! would, so no host starts until it is done.

use std::collections::BTreeMap;
use std::fmt::Write;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use serde_json::{Value, json};

use crate::cli::LogoutArgs;
use crate::credential;
use crate::dashboard::describe::human_size;
use crate::failure::{Failure, code};
use crate::lifecycle;
use crate::login::shell_quoted;
use crate::managed;
use crate::plow::Plow;
use crate::secrets::{Secret, Secrets};
use crate::state::StateDir;
use crate::term;
use crate::vault::store::{Backend, OwnStore};

/// How long logout waits for a `latch plugins` command to let go of the
/// folder; a host never does, and is refused at once.
const HOLD_WITHIN: Duration = Duration::from_secs(60);

pub async fn run(state: &StateDir, args: &LogoutArgs) -> Result<Value, Failure> {
    let refused = |why: &str| {
        let message = format!("{} {why}; nothing was deleted", state.path.display());
        Failure::new(code::state_format, message)
    };
    let found = match fs::symlink_metadata(&state.path) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => None,
        Err(err) => return Err(io_failure(&state.path, err)),
        Ok(found) if found.file_type().is_symlink() => {
            return Err(refused("is a link; name the state folder itself"));
        }
        Ok(_) => {
            let path = state
                .path
                .canonicalize()
                .map_err(|err| io_failure(&state.path, err))?;
            if !path.join("latch.json").is_file() {
                return Err(refused(
                    "has no latch.json, so it is not a latch state folder",
                ));
            }
            Some(path)
        }
    };
    let summary = Summary::of(
        found.as_deref().unwrap_or(&state.path),
        found.is_some(),
        args.purge,
    )?;
    if args.dry_run {
        if term::stderr_is_terminal() {
            eprint!("{}", summary.described(args.purge));
        }
        return Ok(json!({"dry_run": true, "summary": summary}));
    }
    confirm(&summary, args)?;
    let mut output = match found {
        Some(path) => log_out(path, args.purge).await?,
        None => forget_missing(state, args.purge).await?,
    };
    output["summary"] = json!(summary);
    Ok(output)
}

async fn log_out(path: PathBuf, purge: bool) -> Result<Value, Failure> {
    let stopped = managed::stop(&StateDir { path: path.clone() }).await?;
    let serving = |holder: &str| holder.starts_with("latch serve");
    let taken =
        crate::serve::hold_state(&path, lifecycle::LOGGING_OUT, HOLD_WITHIN, |h| !serving(h))?;
    let _lock = match taken {
        Ok(lock) => lock,
        Err(holder) if serving(&holder) => {
            let message = format!(
                "{holder} started on {} as it was stopped; `latch stop`, then log out again",
                path.display()
            );
            return Err(Failure::new(code::host_running, message));
        }
        Err(holder) => {
            let message = format!("{holder} still holds this state folder; run this again");
            return Err(Failure::new(code::state_busy, message));
        }
    };
    held_for_tests();
    let device = unregister(&StateDir { path: path.clone() }).await;
    let secrets = Secrets::open(&path)?;
    let mut removed = secrets.remove_each(if purge { &Secret::ALL } else { &Secret::LOGIN })?;
    if purge {
        fs::remove_dir_all(&path).map_err(|err| io_failure(&path, err))?;
    } else {
        let config = path.join("config.json");
        match fs::remove_file(&config) {
            Ok(()) => removed.push("config.json"),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(io_failure(&config, err)),
        }
    }
    Ok(json!({
        "removed": removed,
        "from": if secrets.in_keychain() { "keychain" } else { "files" },
        "stopped": stopped["stopped"],
        "purged": purge,
        "device": device,
        "state_dir": path,
    }))
}

/// What a logout will do, worked out before it does any of it: what it
/// deletes, by full path, and what is in the vault and audit log it keeps
/// or, with `--purge`, deletes. Counts and names only, never a value.
#[derive(Serialize)]
pub struct Summary {
    state_dir: PathBuf,
    /// Whether it is `~/.latch`, the folder every latch uses by default.
    default_state_dir: bool,
    /// The login this folder has; null when it has none.
    login: Option<Login>,
    /// Whether Plow is asked to take this machine out of the account and
    /// sign out its login: an owner's machine, or a folder that is gone
    /// but left its Plow token.
    leaves_plow: bool,
    host_running: bool,
    /// The Keychain items it deletes, by name (the folder's path and the
    /// secret's file name); none on a build that keeps secrets as files.
    keychain_items: Vec<String>,
    files: Vec<PathBuf>,
    /// Whether the state folder goes too, with everything in it.
    deletes_folder: bool,
    /// The vault's items by kind; null with no vault.
    vault: Option<VaultCount>,
    /// The audit log's records; null with no audit log.
    audit_records: Option<usize>,
    browsers: Browsers,
}

/// The browsers on this folder: each engine installed, a folder per
/// version that `latch browser setup` (or the installer) downloads again,
/// and `browser/`, what sessions left: their journals and screenshots,
/// kept 7 days. A session's profile, sign-ins included, lives there only
/// while it runs; a host removes it when the session ends.
#[derive(Serialize)]
struct Browsers {
    engines: Vec<Engine>,
    /// `browser/`, by size; null when there is none.
    session_data: Option<Sized>,
}

#[derive(Serialize)]
struct Engine {
    /// As `latch browser setup --engine` names it.
    engine: &'static str,
    version: String,
    path: PathBuf,
    bytes: u64,
}

#[derive(Serialize)]
struct Sized {
    path: PathBuf,
    bytes: u64,
}

#[derive(Serialize)]
struct Login {
    name: String,
    kind: String,
}

#[derive(Serialize)]
#[serde(untagged)]
enum VaultCount {
    Items(BTreeMap<&'static str, usize>),
    /// A vault that can't be opened, its key gone, say.
    Unreadable {
        error: String,
    },
}

impl Summary {
    fn of(path: &Path, exists: bool, purge: bool) -> Result<Summary, Failure> {
        let state = StateDir {
            path: path.to_path_buf(),
        };
        let config = if exists { state.load_config()? } else { None };
        let secrets = Secrets::open(path)?;
        let mut keychain_items = Vec::new();
        let mut files = Vec::new();
        for &secret in if purge {
            &Secret::ALL[..]
        } else {
            &Secret::LOGIN[..]
        } {
            if secrets.read(secret)?.is_none() {
                continue;
            }
            let at = path.join(secret.file());
            if secrets.in_keychain() {
                keychain_items.push(at.display().to_string());
            } else {
                files.push(at);
            }
        }
        if !purge && path.join("config.json").exists() {
            files.push(path.join("config.json"));
        }
        let leaves_plow = match &config {
            Some(config) => config.peer.kind == "device",
            None => !exists && state.load_token()?.is_some(),
        };
        let vault = exists.then(|| vault_count(path)).flatten();
        let audit_records = fs::read_to_string(path.join(crate::audit::FILE))
            .ok()
            .map(|log| log.lines().count());
        let default = StateDir::new(None)?.path;
        Ok(Summary {
            state_dir: path.to_path_buf(),
            default_state_dir: default.canonicalize().unwrap_or(default) == path,
            login: config.map(|config| Login {
                name: config.peer.name,
                kind: config.peer.kind,
            }),
            leaves_plow,
            host_running: exists && lifecycle::now(path).running,
            keychain_items,
            files,
            deletes_folder: purge && exists,
            vault,
            audit_records,
            browsers: browsers(path),
        })
    }

    /// The summary as a person reads it, before they confirm.
    fn described(&self, purge: bool) -> String {
        let command = if purge {
            "latch logout --purge"
        } else {
            "latch logout"
        };
        let folder = self.state_dir.display();
        let default = if self.default_state_dir {
            " (the default state folder)"
        } else {
            ""
        };
        let mut text = format!("{command} on {folder}{default} will:\n");
        if self.host_running {
            text += "  - stop the host running on it\n";
        }
        if self.leaves_plow {
            match &self.login {
                Some(login) => {
                    let _ = writeln!(
                        text,
                        "  - take {:?} out of your Plow account and sign out its login",
                        login.name
                    );
                }
                None => {
                    text +=
                        "  - take this machine out of your Plow account and sign out its login\n"
                }
            }
        }
        if !self.keychain_items.is_empty() {
            text += "  - delete these Keychain items:\n";
            for item in &self.keychain_items {
                let _ = writeln!(text, "      latch: {item}");
            }
        }
        if !self.files.is_empty() {
            text += "  - delete these files:\n";
            for file in &self.files {
                let _ = writeln!(text, "      {}", file.display());
            }
        }
        let contents = self.contents();
        if self.deletes_folder {
            let _ = writeln!(text, "  - delete {folder} and everything in it:");
            for line in &contents {
                let _ = writeln!(text, "      {line}");
            }
            text += "      rules, settings, plugins and this machine's key\n";
            if !self.browsers.engines.is_empty() {
                text += "      (`latch browser setup` downloads a browser again)\n";
            }
            text += "This cannot be undone.\n";
        } else if !purge {
            text += "It keeps:\n";
            for line in &contents {
                let _ = writeln!(text, "  - {line}");
            }
            text += "  - rules, settings, plugins and this machine's key";
            if let Some(login) = &self.login {
                let _ = write!(text, ": `latch login` brings it back as {:?}", login.name);
            }
            text += "\n";
        }
        text
    }

    /// The vault and audit log, a line each, as `described` lists them.
    fn contents(&self) -> Vec<String> {
        let mut lines = Vec::new();
        match &self.vault {
            Some(VaultCount::Items(kinds)) if kinds.is_empty() => {
                lines.push("the vault: empty".to_string());
            }
            Some(VaultCount::Items(kinds)) => {
                let counts: Vec<String> = kinds.iter().map(|(kind, n)| counted(*n, kind)).collect();
                lines.push(format!("the vault: {}", counts.join(", ")));
            }
            Some(VaultCount::Unreadable { error }) => {
                lines.push(format!("the vault (unreadable: {error})"));
            }
            None => {}
        }
        if let Some(records) = self.audit_records {
            lines.push(format!("the audit log: {}", counted(records, "record")));
        }
        for engine in &self.browsers.engines {
            let name = match engine.engine {
                "chrome" => "Chrome for Testing",
                _ => "Camoufox",
            };
            lines.push(format!(
                "the {name} {} browser: {} at {}",
                engine.version,
                human_size(engine.bytes),
                engine.path.display()
            ));
        }
        if let Some(data) = &self.browsers.session_data {
            lines.push(format!(
                "browser session journals and screenshots: {} at {}",
                human_size(data.bytes),
                data.path.display()
            ));
        }
        lines
    }
}

fn browsers(path: &Path) -> Browsers {
    let mut engines = Vec::new();
    for (engine, folder) in [("chrome", "cft"), ("camoufox", "camoufox")] {
        let Ok(versions) = fs::read_dir(path.join("plugins").join(folder)) else {
            continue;
        };
        for version in versions.flatten() {
            if version.file_type().is_ok_and(|kind| kind.is_dir()) {
                engines.push(Engine {
                    engine,
                    version: version.file_name().to_string_lossy().into_owned(),
                    path: version.path(),
                    bytes: bytes_in(&version.path()),
                });
            }
        }
    }
    engines.sort_by(|a, b| a.path.cmp(&b.path));
    let data = path.join("browser");
    Browsers {
        engines,
        session_data: data.is_dir().then(|| Sized {
            bytes: bytes_in(&data),
            path: data,
        }),
    }
}

/// The size of every file under `path`, links not followed.
fn bytes_in(path: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => bytes_in(&entry.path()),
            Ok(kind) if kind.is_file() => entry.metadata().map_or(0, |found| found.len()),
            _ => 0,
        })
        .sum()
}

fn vault_count(path: &Path) -> Option<VaultCount> {
    // Opening a folder with no vault would make one.
    if !path.join("vault.json").exists() {
        return None;
    }
    Some(match OwnStore::open(path) {
        Ok(vault) => {
            let mut kinds = BTreeMap::new();
            for item in vault.list() {
                *kinds.entry(item.kind).or_insert(0) += 1;
            }
            VaultCount::Items(kinds)
        }
        Err(failure) => VaultCount::Unreadable {
            error: failure.message,
        },
    })
}

/// `n` and `kind`, made plural for any `n` but 1: "3 logins", "1 card".
fn counted(n: usize, kind: &str) -> String {
    let kind = kind.replace('_', " ");
    match (n, kind.as_str()) {
        (1, _) => format!("1 {kind}"),
        (_, "identity") => format!("{n} identities"),
        _ => format!("{n} {kind}s"),
    }
}

/// Asks before anything is deleted: a person at a terminal says yes (types
/// `delete` for `--purge`); a program passes `--yes`, or is refused with
/// the summary, so it can show it to whoever decides.
fn confirm(summary: &Summary, args: &LogoutArgs) -> Result<(), Failure> {
    let described = summary.described(args.purge);
    if args.yes {
        if term::stderr_is_terminal() {
            eprint!("{described}");
        }
        return Ok(());
    }
    if !term::interactive() {
        let message = "latch logout asks before it deletes anything, and no one is at a \
                       terminal to answer; `summary` says what it would delete. Nothing \
                       was deleted"
            .to_string();
        let mut failure = Failure::new(code::confirmation_required, message);
        let command = if args.purge {
            "latch logout --purge --yes"
        } else {
            "latch logout --yes"
        };
        failure.next = Some(command.into());
        failure.details = json!({"summary": summary});
        return Err(failure);
    }
    eprint!("{described}");
    let agreed = if args.purge {
        term::ask("Type delete to delete all of it: ") == "delete"
    } else {
        term::confirm("Log out?")
    };
    if !agreed {
        let message = "kept everything; nothing was deleted".to_string();
        return Err(Failure::new(code::not_removed, message));
    }
    Ok(())
}

/// Takes this machine out of the Plow account and revokes its login there.
/// Null when there is nothing to take out: no login, or an agent's, whose
/// peer goes with its agent. A failure is reported, not returned, so the
/// local delete still happens.
async fn unregister(state: &StateDir) -> Value {
    let Ok(Some(config)) = state.load_config() else {
        return Value::Null;
    };
    if config.peer.kind != "device" {
        return Value::Null;
    }
    let name = config.peer.name;
    let removed =
        match credential::token(state).and_then(|token| Plow::new(&config.api_url, &token)) {
            Ok(plow) => plow.remove_self().await,
            Err(failure) => Err(failure),
        };
    match removed {
        Ok(_) => json!({"name": name, "removed": true}),
        Err(failure) => json!({
            "name": name,
            "removed": false,
            "error": {"code": failure.code, "message": failure.message},
            "next": format!("`latch devices remove {}` on another of your machines", shell_quoted(&name)),
        }),
    }
}

/// Logout of a folder that is gone: no host runs on it and no config says
/// which Plow it logged in to, but a signed latch's Keychain may still hold
/// its secrets.
async fn forget_missing(state: &StateDir, purge: bool) -> Result<Value, Failure> {
    let device = match state.load_token()? {
        Some(token) => unregister_by_token(&token).await,
        None => Value::Null,
    };
    let secrets = Secrets::open(&state.path)?;
    let removed = secrets.remove_each(if purge { &Secret::ALL } else { &Secret::LOGIN })?;
    Ok(json!({
        "removed": removed,
        "from": if secrets.in_keychain() { "keychain" } else { "files" },
        "stopped": false,
        "purged": purge,
        "device": device,
        "state_dir": state.path,
    }))
}

/// `unregister` with only the token: the Plow is the one a login with no
/// saved URL would use, and the machine's name is what Plow says it removed.
async fn unregister_by_token(token: &str) -> Value {
    let api_url = crate::login::api_url(None, credential::env, None, crate::cli::BUILD_CHANNEL);
    let removed = match api_url {
        Some(api_url) => match Plow::new(&api_url, token) {
            Ok(plow) => plow.remove_self().await,
            Err(failure) => Err(failure),
        },
        None => Err(Failure::new(
            code::not_configured,
            "no Plow URL to take this machine out of".to_string(),
        )),
    };
    match removed {
        Ok(peers) => json!({
            "name": peers.first().map(|peer| peer.name.clone()),
            "removed": true,
        }),
        Err(failure) => json!({
            "name": Value::Null,
            "removed": false,
            "error": {"code": failure.code, "message": failure.message},
            "next": "`latch devices` on another of your machines lists it, and `latch devices remove NAME` takes it out",
        }),
    }
}

/// With `LATCH_TEST_LOGOUT_HELD` naming a file, a test build writes it
/// once logout holds the folder, and waits until the test deletes it.
fn held_for_tests() {
    if !cfg!(debug_assertions) {
        return;
    }
    let Some(file) = std::env::var_os("LATCH_TEST_LOGOUT_HELD") else {
        return;
    };
    let _ = fs::write(&file, "held");
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while Path::new(&file).exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn io_failure(path: &Path, err: io::Error) -> Failure {
    Failure::new(code::state_io, format!("{}: {err}", path.display()))
}

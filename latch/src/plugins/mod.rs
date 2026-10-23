//! The connectors that ship inside latch (Google; on macOS, Messages and
//! Contacts), each turned on per host by its owner. `latch plugins enable
//! NAME` runs the connector's setup on the running host, or in the command
//! itself when no host runs, and the connector counts as enabled only once
//! that setup succeeds. Until then, and from the moment `disable` starts,
//! its operations get `not_enabled` before anything is judged or any worker
//! starts.
//!
//! Which connectors are on is kept in `<state-dir>/plugins.json`, written
//! by the running host, or by the command when none runs. The browser is
//! core, like exec and files, and never a plugin.

pub mod client;
pub mod host;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Notify;

use crate::failure::{Exit, Failure};
use crate::state::StateDir;

/// One connector. An operation of the same name belongs to it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Connector {
    Google,
    #[cfg(target_os = "macos")]
    Messages,
    #[cfg(target_os = "macos")]
    Contacts,
}

impl Connector {
    /// Every connector this build has, in the order `list` shows them.
    pub const ALL: &[Connector] = &[
        Connector::Google,
        #[cfg(target_os = "macos")]
        Connector::Messages,
        #[cfg(target_os = "macos")]
        Connector::Contacts,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Connector::Google => "google",
            #[cfg(target_os = "macos")]
            Connector::Messages => "messages",
            #[cfg(target_os = "macos")]
            Connector::Contacts => "contacts",
        }
    }

    pub fn named(name: &str) -> Result<Connector, Failure> {
        Self::ALL
            .iter()
            .copied()
            .find(|connector| connector.name() == name)
            .ok_or_else(|| {
                let names: Vec<&str> = Self::ALL.iter().map(|c| c.name()).collect();
                let message = format!("no plugin {name:?} here; plugins: {}", names.join(", "));
                Failure::usage("unknown_plugin", message)
            })
    }

    /// The connector a host operation belongs to, if any.
    pub fn of_op(op: &str) -> Option<Connector> {
        Self::ALL.iter().copied().find(|c| c.name() == op)
    }

    /// What the owner runs, on the host, to turn this connector on.
    fn enable_command(self) -> String {
        format!("latch plugins enable {}", self.name())
    }

    fn not_enabled(self) -> Failure {
        let mut failure = Failure::new(
            Exit::Denied,
            "not_enabled",
            format!(
                "{} is not enabled on this host; its owner turns it on there with `{}`",
                self.name(),
                self.enable_command()
            ),
        );
        failure.details = json!({"plugin": self.name(), "enable": self.enable_command()});
        failure
    }
}

/// A connector's saved setting. One with none is disabled.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum Setting {
    Enabled,
    /// Its last setup did not succeed, for `reason`.
    NeedsSetup {
        reason: String,
    },
}

/// A host's connectors: what each is set to, and how many of each one's
/// operations are running now, so `disable` can wait for them.
pub struct Plugins {
    path: PathBuf,
    /// The home folder of the user the host runs as: where the Messages
    /// and Contacts setups look.
    #[cfg_attr(not(target_os = "macos"), expect(dead_code))]
    home: PathBuf,
    inner: Mutex<Inner>,
    /// Woken each time an operation ends.
    ended: Notify,
}

#[derive(Default)]
struct Inner {
    settings: BTreeMap<Connector, Setting>,
    running: BTreeMap<Connector, usize>,
    /// How many times each connector's setting has changed, so a setup can
    /// tell whether it was turned on or off while the setup ran.
    revisions: BTreeMap<Connector, u64>,
}

impl Plugins {
    /// The settings saved in `state`'s `plugins.json`; none if there is no
    /// such file yet.
    pub fn load(state: &StateDir, home: &Path) -> Result<Plugins, Failure> {
        let settings = saved(state)?;
        Ok(Plugins {
            path: state.path.join("plugins.json"),
            home: home.to_path_buf(),
            inner: Mutex::new(Inner {
                settings,
                ..Inner::default()
            }),
            ended: Notify::new(),
        })
    }

    /// Lets one of `connector`'s operations start, if it is enabled. It
    /// counts as running until the returned guard is dropped.
    pub fn start(&self, connector: Connector) -> Result<Running<'_>, Failure> {
        let mut inner = self.inner.lock().expect("plugins lock");
        if inner.settings.get(&connector) != Some(&Setting::Enabled) {
            return Err(connector.not_enabled());
        }
        *inner.running.entry(connector).or_default() += 1;
        Ok(Running {
            plugins: self,
            connector,
        })
    }

    /// `connector`'s revision now: what a setup passes to `set` when done.
    fn revision(&self, connector: Connector) -> u64 {
        let inner = self.inner.lock().expect("plugins lock");
        inner.revisions.get(&connector).copied().unwrap_or(0)
    }

    /// Saves `connector`'s new setting (none: disabled), then uses it; with
    /// `since`, only if the setting has not changed since that revision.
    /// Returns how many of its operations are running at that moment.
    fn set(
        &self,
        connector: Connector,
        setting: Option<Setting>,
        since: Option<u64>,
    ) -> Result<usize, Failure> {
        let mut inner = self.inner.lock().expect("plugins lock");
        let revision = inner.revisions.entry(connector).or_default();
        if since.is_some_and(|since| since != *revision) {
            let message = format!(
                "{} was turned on or off while its setup ran; run `{}` again to set it up",
                connector.name(),
                connector.enable_command()
            );
            return Err(Failure::failed("setup_superseded", message));
        }
        *revision += 1;
        let mut settings = inner.settings.clone();
        match setting {
            Some(setting) => settings.insert(connector, setting),
            None => settings.remove(&connector),
        };
        let text = serde_json::to_string_pretty(&settings).expect("settings serialize") + "\n";
        crate::state::write_private(&self.path, text.as_bytes()).map_err(|err| {
            Failure::failed("bad_state", format!("{}: {err}", self.path.display()))
        })?;
        inner.settings = settings;
        Ok(inner.running.get(&connector).copied().unwrap_or(0))
    }

    /// Finishes once none of `connector`'s operations is running.
    async fn idle(&self, connector: Connector) {
        loop {
            // Listening before looking, so an operation that ends in
            // between still wakes us.
            let ended = self.ended.notified();
            tokio::pin!(ended);
            ended.as_mut().enable();
            let running = self
                .inner
                .lock()
                .expect("plugins lock")
                .running
                .get(&connector)
                .copied();
            if running.unwrap_or(0) == 0 {
                return;
            }
            ended.await;
        }
    }

    /// Each connector and whether its owner turned it on: what is known
    /// here, which is not whether it works now.
    pub fn enabled(&self) -> Vec<(String, String)> {
        let inner = self.inner.lock().expect("plugins lock");
        Connector::ALL
            .iter()
            .map(|&connector| {
                let state = match inner.settings.get(&connector) {
                    Some(Setting::Enabled) => "enabled",
                    _ => "disabled",
                };
                (connector.name().to_string(), state.to_string())
            })
            .collect()
    }

    fn list(&self) -> Value {
        listed(&self.inner.lock().expect("plugins lock").settings)
    }
}

/// Every connector with its state, and for one not enabled, why and the
/// owner's command that turns it on.
fn listed(settings: &BTreeMap<Connector, Setting>) -> Value {
    let plugins: Vec<Value> = Connector::ALL
        .iter()
        .map(|&connector| {
            let name = connector.name();
            match settings.get(&connector) {
                Some(Setting::Enabled) => json!({"name": name, "state": "enabled"}),
                Some(Setting::NeedsSetup { reason }) => json!({"name": name,
                    "state": "needs_setup", "reason": reason,
                    "enable": connector.enable_command()}),
                None => json!({"name": name, "state": "disabled",
                    "reason": "its owner has not enabled it on this host",
                    "enable": connector.enable_command()}),
            }
        })
        .collect();
    json!({"plugins": plugins})
}

/// The settings saved in `state`'s `plugins.json`, which a running host
/// keeps up to date and a host-less `enable` or `disable` writes; none if
/// there is no such file yet.
pub(crate) fn saved(state: &StateDir) -> Result<BTreeMap<Connector, Setting>, Failure> {
    let path = state.path.join("plugins.json");
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).map_err(|err| {
            Failure::failed(
                "bad_state",
                format!("{} is not valid: {err}", path.display()),
            )
        }),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(err) => Err(Failure::failed(
            "bad_state",
            format!("{}: {err}", path.display()),
        )),
    }
}

/// `list` for this machine, from `state`'s saved settings: the same answer
/// its host gives while it runs, since it saves each change first.
pub fn list_saved(state: &StateDir) -> Result<Value, Failure> {
    Ok(listed(&saved(state)?))
}

/// One running operation of a connector; it ends when this is dropped.
pub struct Running<'a> {
    plugins: &'a Plugins,
    connector: Connector,
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        let mut inner = self.plugins.inner.lock().expect("plugins lock");
        *inner
            .running
            .get_mut(&self.connector)
            .expect("a running operation is counted") -= 1;
        self.plugins.ended.notify_waiters();
    }
}

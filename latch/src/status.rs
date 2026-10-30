//! `latch status`: who this machine is, whether it hosts, and the account
//! and hosts Plow lists for it. With `--local`, only what this machine knows,
//! for an app (see `local`); with `--follow` too, again each time that
//! changes (see `follow`).

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use serde_json::{Value, json};

use crate::credential;
use crate::failure::{Failure, code};
use crate::hosts;
use crate::lifecycle::{self, Phase, Problem};
use crate::plow::{Peer, Plow};
use crate::state::StateDir;
use crate::watch::{Watch, Woke};
use crate::wire::Request;

/// How long the account's hosts have, all together, to say which plugins
/// they have on, from before the first is dialed.
pub const PLUGINS_WITHIN: Duration = Duration::from_secs(2);

#[derive(Serialize)]
pub struct Status {
    /// The account this machine's login belongs to; `null` before login.
    pub account: Option<Account>,
    #[serde(flatten)]
    pub local: LocalStatus,
    /// The account's hosts as Plow lists them, this machine first; `null`
    /// before login.
    pub hosts: Option<Vec<Host>>,
    /// The host a command without `--host` goes to, or `null` with the
    /// reason in `default_host_reason`.
    pub default_host: Option<String>,
    pub default_host_reason: Option<String>,
}

#[derive(Serialize)]
pub struct Account {
    pub owner_uid: String,
    pub display_name: Option<String>,
}

/// A host as Plow lists it: whether it is online and when Plow last heard
/// from it are Plow's report, not this machine's.
#[derive(Serialize)]
pub struct Host {
    pub name: String,
    pub key: String,
    /// The machine whose status this is.
    #[serde(rename = "self")]
    pub this_machine: bool,
    pub online: Option<bool>,
    pub last_seen_at: Option<String>,
    /// Its plugins that are on or need setup, as it says when asked (this
    /// machine's from its own state folder); `null` when that is unknown,
    /// with why in `plugins_reason`.
    pub plugins: Option<Vec<Plugin>>,
    /// `host_offline` (not asked), `host_unreachable` (no answer in time),
    /// `caller_denied`, `protocol_mismatch` or the code the host failed with.
    pub plugins_reason: Option<String>,
}

/// One of a host's plugins, as `hosts[]` lists it: never one that is off.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Plugin {
    pub name: String,
    /// `enabled` or `needs_setup`: the owner's saved setting, not a check
    /// that it works.
    pub state: String,
}

/// The plugins in a host's list (its `plugins` request's answer) that are
/// on or need setup.
pub fn on_or_set_up(listed: &Value) -> Vec<Plugin> {
    listed["plugins"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|plugin| {
            let state = plugin["state"].as_str()?;
            (state == "enabled" || state == "needs_setup").then(|| Plugin {
                name: plugin["name"].as_str().unwrap_or_default().to_string(),
                state: state.to_string(),
            })
        })
        .collect()
}

/// How long `status` waits for Plow's answer.
const PLOW_WITHIN: std::time::Duration = std::time::Duration::from_secs(10);

/// `status`: `--local`'s answer, and what Plow says of the account. Before
/// login it asks no one; after, a failed request fails the command.
pub async fn run(state: &StateDir) -> Result<Status, Failure> {
    let local = local(state)?;
    let Some(config) = state.load_config()? else {
        return Ok(Status {
            account: None,
            local,
            hosts: None,
            default_host: None,
            default_host_reason: Some(code::not_logged_in.name.to_string()),
        });
    };
    let directory = tokio::time::timeout(PLOW_WITHIN, directory(state))
        .await
        .unwrap_or_else(|_| Err(crate::plow::no_answer(&config.api_url, PLOW_WITHIN)));
    let mut directory = directory.map_err(|mut failure| {
        if failure.code == code::plow_unreachable.name {
            failure.next = Some("latch status --local".into());
        }
        failure
    })?;
    let hosts = &mut directory.hosts;
    ask_plugins(state, &directory.peers, |index, answer| match answer {
        Ok(plugins) => hosts[index].plugins = Some(plugins),
        Err(reason) => hosts[index].plugins_reason = Some(reason),
    })
    .await;
    Ok(Status {
        account: Some(directory.account),
        local,
        hosts: Some(directory.hosts),
        default_host: directory.default_host,
        default_host_reason: directory.default_host_reason,
    })
}

/// What Plow says of the account this machine is logged in to.
pub struct Directory {
    pub account: Account,
    /// This machine first; the rest in Plow's order. Their plugins are not
    /// asked yet (`ask_plugins`).
    pub hosts: Vec<Host>,
    /// The same hosts, as Plow lists them, to dial.
    pub peers: Vec<Peer>,
    pub default_host: Option<String>,
    pub default_host_reason: Option<String>,
}

/// Asks Plow, all at once, who the account is and which hosts it has, and
/// picks the host a command without `--host` goes to, as `hosts::resolve`
/// would, without registering or dialing anything.
pub async fn directory(state: &StateDir) -> Result<Directory, Failure> {
    let config = state.require_login()?;
    let plow = Plow::new(&config.api_url, &credential::token(state)?)?;
    let (peers, owner_uid, display_name) =
        tokio::try_join!(plow.list_peers(), plow.owner_uid(), plow.display_name())?;
    let mut listed = hosts::only_hosts(peers, &config.peer.key);
    listed.sort_by_key(|host| !host.this_machine);
    let named = hosts::named(state, std::env::var("LATCH_HOST").ok().as_deref())?;
    let asked = named.host();
    let default = if let hosts::Named::NoDefault = named {
        Err(Failure::new(code::no_default_host, String::new()))
    } else if hosts::this_machine_chosen(state, &config, asked) {
        Ok(config.peer.name.clone())
    } else {
        hosts::pick(asked, &listed).map(|host| host.name)
    };
    let (default_host, default_host_reason) = match default {
        Ok(name) => (Some(name), None),
        Err(failure) => (None, Some(failure.code)),
    };
    Ok(Directory {
        account: Account {
            owner_uid,
            display_name,
        },
        hosts: listed
            .iter()
            .map(|peer| Host {
                name: peer.name.clone(),
                key: peer.key.clone(),
                this_machine: peer.this_machine,
                online: peer.online,
                last_seen_at: peer.last_seen_at.clone(),
                plugins: None,
                plugins_reason: None,
            })
            .collect(),
        peers: listed,
        default_host,
        default_host_reason,
    })
}

/// Asks each of `hosts` which plugins it has on, all at once within
/// `PLUGINS_WITHIN`, and gives `each` a host's answer, or the code of why
/// there is none, by its place in `hosts`, as it comes. This machine's is
/// read from its state folder, never dialed. Asking no host fails this.
pub async fn ask_plugins(
    state: &StateDir,
    hosts: &[Peer],
    mut each: impl FnMut(usize, Result<Vec<Plugin>, String>),
) {
    let mut others = Vec::new();
    for (index, host) in hosts.iter().enumerate() {
        if !host.this_machine {
            others.push(index);
            continue;
        }
        match crate::plugins::list_saved(state) {
            Ok(listed) => each(index, Ok(on_or_set_up(&listed))),
            Err(failure) => each(index, Err(failure.code)),
        }
    }
    let peers: Vec<Peer> = others.iter().map(|&index| hosts[index].clone()).collect();
    let request = Request {
        op: "plugins".to_string(),
        args: json!({"command": "list"}),
        chat: None,
    };
    crate::dial::ask_each(state, &peers, &request, PLUGINS_WITHIN, |at, answer| {
        each(
            others[at],
            answer
                .map(|listed| on_or_set_up(&listed))
                .map_err(|failure| failure.code),
        )
    })
    .await;
}

#[derive(Serialize)]
pub struct LocalStatus {
    /// `{"kind": "device" | "agent"}`, Plow's word for this login; `null`
    /// before login.
    pub login: Option<Login>,
    pub this_machine: ThisMachine,
    /// Each connector: `name` and `state` (enabled, disabled or
    /// needs_setup), and for one not enabled, why and the owner's `enable`
    /// (on an agent's login, why alone).
    pub plugins: Value,
    /// Whether a newer build is available, as the host last looked.
    pub update: crate::update::Status,
    /// Why the host last stopped, when it stopped by a failure.
    pub last_error: Option<Problem>,
    /// The Plow this machine logged in to; `null` before login.
    pub api_url: Option<String>,
    /// The host `--host default` names, as `latch hosts default` saved it
    /// in this state folder; `null` when none is.
    pub saved_default_host: Option<String>,
    pub state_dir: PathBuf,
    pub version: &'static str,
    pub protocol: u32,
    /// An older latch's plaintext audit log in the state folder, which this
    /// latch never reads: it can be deleted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_audit_log: Option<PathBuf>,
    /// Said only where Chrome runs without its own sandbox
    /// (`sandbox::page_isolation_off`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_isolation: Option<&'static str>,
}

#[derive(Serialize)]
pub struct Login {
    pub kind: String,
}

/// The machine whose status this is (with `--host NAME`, NAME): the name
/// and key `serve` registers it with (`null` before login), and whether it
/// hosts.
#[derive(Serialize)]
pub struct ThisMachine {
    pub name: Option<String>,
    pub key: Option<String>,
    pub hosting: Hosting,
    /// Windows: whether agents may list folders here (`granted`, `missing`
    /// or `partial`), as `latch volume-roots` lets them. Not said elsewhere.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub list_folders: Option<&'static str>,
}

#[derive(Serialize)]
pub struct Hosting {
    /// A `latch serve` holds this state folder.
    pub running: bool,
    /// It is online and accepting callers: this latch can reach it.
    pub ready: bool,
    /// No host runs; a `latch plugins` command is changing the connectors.
    pub configuring: bool,
    /// It runs as this user's service, as `latch serve` and `serve -d`
    /// start it, rather than in the foreground (`serve --foreground`).
    pub managed: bool,
    /// A managed host held by the `latch serve` in a terminal that started
    /// it, rather than one `serve -d` started.
    pub attached: bool,
    pub state: Phase,
    pub pid: Option<u32>,
    /// Why a host that runs cannot be used: one from another latch version.
    pub blocked: Option<Problem>,
    /// What a starting host waits for, while it does.
    pub waiting: Option<String>,
    /// The agents refused `exec` here (`serve --deny-exec`): as the host
    /// runs, or as the last one ran; `null` if its saved flags are unreadable.
    pub deny_exec: Option<crate::serve::DenyExec>,
    /// Whether the host keeps this machine awake (`latch settings set
    /// --keep-awake`): the setting, and what the host holds; `null` if the
    /// saved settings are unreadable.
    pub keep_awake: Option<crate::keep_awake::KeepAwake>,
}

/// `status --local`: reads the state folder and asks no one, so it works
/// offline and before login. Not logged in and not serving are answers.
pub fn local(state: &StateDir) -> Result<LocalStatus, Failure> {
    let config = state.load_config()?;
    let mut plugins = crate::plugins::list_saved(state)?;
    // Only the owner's own login turns plugins on: an agent's has no
    // command to run.
    if config
        .as_ref()
        .is_some_and(|config| config.peer.kind != "device")
    {
        for plugin in plugins["plugins"].as_array_mut().into_iter().flatten() {
            let off = plugin.as_object_mut().and_then(|p| p.remove("enable"));
            if off.is_some() {
                plugin["reason"] = json!("only the owner's login can turn this plugin on");
            }
        }
    }
    let dir = state.path.canonicalize().unwrap_or(state.path.clone());
    let Hosted {
        hosting,
        last_error,
    } = hosted(state, None).expect("with no deadline the service manager is waited for");
    Ok(LocalStatus {
        login: config.as_ref().map(|config| Login {
            kind: config.peer.kind.clone(),
        }),
        this_machine: ThisMachine {
            name: config.as_ref().map(|config| config.peer.name.clone()),
            key: config.as_ref().map(|config| config.peer.key.clone()),
            hosting,
            list_folders: crate::sandbox::list_folders(),
        },
        plugins: plugins["plugins"].take(),
        update: crate::update::status(&dir),
        last_error,
        api_url: config.map(|config| config.api_url),
        saved_default_host: hosts::saved_default(state)?,
        state_dir: dir.clone(),
        version: crate::cli::version(),
        protocol: crate::wire::protocol(),
        old_audit_log: crate::audit::plaintext(&dir),
        page_isolation: crate::sandbox::page_isolation_off()
            .then_some(crate::sandbox::PAGE_ISOLATION_OFF),
    })
}

/// What latch knows of the host on a state folder, and why it last stopped
/// when a failure stopped it.
pub struct Hosted {
    pub hosting: Hosting,
    pub last_error: Option<Problem>,
}

/// The part of `--local` that can wait: on the service manager
/// (`launchctl`, `systemctl`), and up to half a second on a running host's
/// socket. With `until`, `None` if the service manager has not answered by
/// then; its command is killed and reaped.
pub fn hosted(state: &StateDir, until: Option<std::time::Instant>) -> Option<Hosted> {
    let host = lifecycle::now(&state.path);
    let blocked = lifecycle::other_version(&state.path).map(|pid| {
        let failure = lifecycle::other_version_running(pid);
        Problem {
            code: failure.code,
            message: failure.message,
        }
    });
    let dir = state.path.canonicalize().unwrap_or(state.path.clone());
    let managed = host.running && crate::managed::Service::new(&dir).registered_by(until)?;
    Some(Hosted {
        hosting: Hosting {
            running: host.running,
            ready: host.state == Phase::Serving && blocked.is_none(),
            configuring: host.configuring,
            managed,
            attached: host.attached,
            state: host.state,
            pid: host.pid,
            blocked,
            waiting: host.waiting.clone(),
            deny_exec: crate::managed::saved_deny_exec(&dir),
            keep_awake: crate::keep_awake::status(&dir, &host).ok(),
        },
        last_error: host.error,
    })
}

/// `status --local --follow`: `hello`, then this machine's `status --local`
/// as a `status` line, and again each time it changes, given to `send`
/// until it returns false (or `hangup` closes). It sleeps until the state
/// folder or the host changes, and never opens the audit. A host streams
/// its own to its owner's `status --host --follow` this way.
pub fn follow(
    dir: &Path,
    hangup: Option<crate::watch::Hangup>,
    mut send: impl FnMut(&Value) -> bool,
) -> Result<(), Failure> {
    let watching =
        |err: std::io::Error| Failure::new(code::watch_failed, format!("{}: {err}", dir.display()));
    let state = StateDir {
        path: dir.to_path_buf(),
    };
    let lock = dir.join("serve.lock");
    let mut watch = Watch::new(dir, &[lock], hangup).map_err(watching)?;
    if !send(&crate::stream::hello()) {
        return Ok(());
    }
    let mut sent = None;
    loop {
        watch.arm().map_err(watching)?;
        let status = local(&state)?;
        watch.process(status.this_machine.hosting.pid);
        let status = serde_json::to_value(status).expect("a status serializes");
        if sent.as_ref() != Some(&status) {
            if !send(&json!({"type": "status", "status": status})) {
                return Ok(());
            }
            sent = Some(status);
        }
        match watch.wait().map_err(watching)? {
            Woke::HungUp => return Ok(()),
            Woke::Changed => {}
        }
    }
}

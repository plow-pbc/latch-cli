//! `latch status`: who this machine is, whether it hosts, and the account
//! and hosts Plow lists for it. With `--local`, only what this machine knows,
//! for an app (see `local`); with `--follow` too, again each time that
//! changes (see `follow`).

use std::os::fd::RawFd;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Value, json};

use crate::credential;
use crate::failure::{Failure, code};
use crate::hosts;
use crate::lifecycle::{self, Phase, Problem};
use crate::plow::Plow;
use crate::state::StateDir;
use crate::watch::{Watch, Woke};

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
}

/// `status`: `--local`'s answer, and what Plow says of the account. Before
/// login it asks no one; after, a failed request fails the command.
pub async fn run(state: &StateDir) -> Result<Status, Failure> {
    let local = local(state)?;
    if state.load_config()?.is_none() {
        return Ok(Status {
            account: None,
            local,
            hosts: None,
            default_host: None,
            default_host_reason: Some(code::not_logged_in.name.to_string()),
        });
    }
    let directory = directory(state).await.map_err(|mut failure| {
        if failure.code == code::plow_unreachable.name {
            failure.next = Some("latch status --local".into());
        }
        failure
    })?;
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
    /// This machine first; the rest in Plow's order.
    pub hosts: Vec<Host>,
    pub default_host: Option<String>,
    pub default_host_reason: Option<String>,
}

/// Asks Plow, all at once, who the account is and which hosts it has, and
/// picks the host a command without `--host` goes to, as `hosts::resolve`
/// would, without registering or dialing anything.
pub async fn directory(state: &StateDir) -> Result<Directory, Failure> {
    let config = state.require_login()?;
    let plow = Plow::new(&config.api_url, &credential::token(state)?);
    let (peers, owner_uid, display_name) =
        tokio::try_join!(plow.list_peers(), plow.owner_uid(), plow.display_name())?;
    let mut listed = hosts::only_hosts(peers, &config.peer.key);
    listed.sort_by_key(|host| !host.this_machine);
    let asked = std::env::var("LATCH_HOST").ok();
    let default = if hosts::this_machine_chosen(state, &config, asked.as_deref()) {
        Ok(config.peer.name.clone())
    } else {
        hosts::pick(asked.as_deref(), &listed).map(|host| host.name)
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
            .into_iter()
            .map(|peer| Host {
                name: peer.name,
                key: peer.key,
                this_machine: peer.this_machine,
                online: peer.online,
                last_seen_at: peer.last_seen_at,
            })
            .collect(),
        default_host,
        default_host_reason,
    })
}

#[derive(Serialize)]
pub struct LocalStatus {
    /// `{"kind": "device" | "agent"}`, Plow's word for this login; `null`
    /// before login.
    pub login: Option<Login>,
    pub this_machine: ThisMachine,
    /// As `latch plugins list` says.
    pub plugins: Value,
    /// Whether a newer build is available, as the host last looked.
    pub update: crate::update::Status,
    /// Why the host last stopped, when it stopped by a failure.
    pub last_error: Option<Problem>,
    /// The Plow this machine logged in to; `null` before login.
    pub api_url: Option<String>,
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
    /// The agents refused `exec` here (`serve --deny-exec`): as the host
    /// runs, or as the last one ran; `null` if its saved flags are unreadable.
    pub deny_exec: Option<crate::serve::DenyExec>,
}

/// `status --local`: reads the state folder and asks no one, so it works
/// offline and before login. Not logged in and not serving are answers.
pub fn local(state: &StateDir) -> Result<LocalStatus, Failure> {
    let host = lifecycle::now(&state.path);
    let blocked = lifecycle::other_version(&state.path).map(|pid| {
        let failure = lifecycle::other_version_running(pid);
        Problem {
            code: failure.code,
            message: failure.message,
        }
    });
    let config = state.load_config()?;
    let mut plugins = crate::plugins::list_saved(state)?;
    let dir = state.path.canonicalize().unwrap_or(state.path.clone());
    Ok(LocalStatus {
        login: config.as_ref().map(|config| Login {
            kind: config.peer.kind.clone(),
        }),
        this_machine: ThisMachine {
            name: config.as_ref().map(|config| config.peer.name.clone()),
            key: config.as_ref().map(|config| config.peer.key.clone()),
            hosting: Hosting {
                running: host.running,
                ready: host.state == Phase::Serving && blocked.is_none(),
                configuring: host.configuring,
                managed: host.running && crate::managed::Service::new(&dir).registered(),
                attached: host.attached,
                state: host.state,
                pid: host.pid,
                blocked,
                deny_exec: crate::managed::saved_deny_exec(&dir),
            },
        },
        plugins: plugins["plugins"].take(),
        update: crate::update::status(&dir),
        last_error: host.error,
        api_url: config.map(|config| config.api_url),
        state_dir: dir.clone(),
        version: crate::cli::version(),
        protocol: crate::wire::protocol(),
        old_audit_log: crate::audit::plaintext(&dir),
        page_isolation: crate::sandbox::page_isolation_off()
            .then_some(crate::sandbox::PAGE_ISOLATION_OFF),
    })
}

/// `status --local --follow`: `hello`, then this machine's `status --local`
/// as a `status` line, and again each time it changes, given to `send`
/// until it returns false (or `hangup` closes). It sleeps until the state
/// folder or the host changes, and never opens the audit. A host streams
/// its own to its owner's `status --host --follow` this way.
pub fn follow(
    dir: &Path,
    hangup: Option<RawFd>,
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

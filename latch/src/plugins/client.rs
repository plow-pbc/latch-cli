//! `latch plugins …`. `list` asks another host named by `--host`; for this
//! machine it reads the state folder, running host or not, as `rules show`
//! does. `enable` and `disable` are for this machine only, and only from
//! the owner's own login: managing another host's connectors from here is
//! not a thing latch does. They go to this machine's host over its private
//! socket when it runs; when none does, this command makes the same change
//! itself, in the state folder the next `latch serve` reads.

use std::time::Duration;

use serde_json::{Value, json};

use super::{Connector, Plugins, host};
use crate::cli::{PluginsArgs, PluginsCommand};
use crate::dial::{self, ANSWER_WITHIN};
use crate::failure::{Exit, Failure};
use crate::hosts;
use crate::lifecycle;
use crate::plow::{Peer, Plow};
use crate::state::StateDir;
use crate::wire::Request;

/// How long an offline `enable` or `disable` waits for another one on the
/// same state folder: past the Full Disk Access check's own limit.
const HOLD_WITHIN: Duration = Duration::from_secs(60);

pub async fn run(state: &StateDir, args: PluginsArgs) -> Result<Value, Failure> {
    let events = matches!(args.command, PluginsCommand::Enable { events: true, .. });
    let adding = matches!(
        args.command,
        PluginsCommand::Enable {
            add_account: true,
            ..
        }
    );
    let (host, args) = match args.command {
        PluginsCommand::List { host } if hosts::local_target(state, host.as_deref())? => {
            return super::list_saved(state);
        }
        PluginsCommand::List { host } => (
            hosts::resolve(state, host).await?,
            json!({"command": "list"}),
        ),
        PluginsCommand::Enable {
            name, add_account, ..
        } => {
            if add_account && Connector::named(&name)? != Connector::Google {
                let message = "only Google takes --add-account".to_string();
                return Err(Failure::usage("usage", message));
            }
            (
                this_host(state)?,
                json!({"command": "enable", "name": name}),
            )
        }
        PluginsCommand::Disable { name } => (
            this_host(state)?,
            json!({"command": "disable", "name": name}),
        ),
    };
    let request = Request {
        op: "plugins".to_string(),
        args,
        chat: None,
    };
    let send = || async {
        match dial::call(state, &host, &request, ANSWER_WITHIN).await {
            Err(failure)
                if failure.code == "serve_required" && request.args["name"].is_string() =>
            {
                without_a_host(state, &request.args).await
            }
            answer => answer,
        }
    };
    // Signing in waits here, outside any host call or hold on the state
    // folder; a stop asked for meanwhile answers null, for nothing to print.
    let signed_in = match send().await {
        Err(failure) if failure.code == "google_not_connected" => {
            super::connect::google(state, Some(failure), None, events, send).await
        }
        Ok(result) if adding => {
            super::connect::google(state, None, Some(result), events, send).await
        }
        answer => return answer,
    };
    signed_in.map(Option::unwrap_or_default)
}

/// `enable` or `disable` with no host running: the host's own setup and
/// save, made here while holding the state folder as a host would, so no
/// host starts, and no other `plugins` command saves, in between.
async fn without_a_host(state: &StateDir, args: &Value) -> Result<Value, Failure> {
    let dir = state.path.canonicalize().unwrap_or(state.path.clone());
    let command = args["command"].as_str().unwrap_or_default();
    let connector = Connector::named(args["name"].as_str().unwrap_or_default())?;
    if let Some(pid) = lifecycle::other_version(&dir) {
        return Err(lifecycle::other_version_running(pid));
    }
    let who = format!("{}{command} {}", lifecycle::CONFIGURING, connector.name());
    // Another `plugins` command lets go once it is done; a host never does.
    let serving = |holder: &str| holder.starts_with("latch serve");
    let _held = match crate::serve::hold_state(&dir, &who, HOLD_WITHIN, |h| !serving(h))? {
        Ok(held) => held,
        Err(holder) if serving(&holder) => {
            // A host of this latch's, not answering yet.
            let message = format!("{holder} is starting on this state folder; run this again");
            return Err(Failure::new(Exit::Unreachable, "serve_required", message));
        }
        Err(holder) => {
            let message = format!("{holder} still holds this state folder; run this again");
            return Err(Failure::failed("state_busy", message));
        }
    };
    let workers = crate::serve::workers(dir)?;
    let plugins = Plugins::load(state, &workers.home)?;
    if command == "enable" {
        let plow = Plow::from_state(state)?;
        host::enable(&plugins, &workers, &plow, connector, true).await
    } else {
        host::disable(&plugins, connector).await
    }
}

/// This machine's own host, when the owner is the one asking. An agent's
/// login never sends the command at all.
fn this_host(state: &StateDir) -> Result<Peer, Failure> {
    let config = state.require_config()?;
    if config.peer.kind != "device" {
        let message = "plugins are turned on and off by the owner, on the host itself";
        return Err(Failure::new(Exit::Denied, "owner_only", message.into()));
    }
    Ok(Peer {
        this_machine: true,
        ..config.peer
    })
}

//! The `plugins` operation: `list` for any caller the host lets in;
//! `enable` and `disable` only from the owner at this host itself, over its
//! private socket. Neither is ever judged: the owner's commands on the host
//! are the owner's, and an agent gets `owner_only` for both.

#[cfg(target_os = "macos")]
use std::path::Path;

use std::os::unix::fs::PermissionsExt;

use serde_json::{Map, Value, json};

use super::{Connector, Plugins, Setting};
use crate::failure::{Exit, Failure};
use crate::plow::Plow;
use crate::serve::Done;
use crate::worker::WorkerSetup;

/// One `plugins` command. `local` says it came over the host's private
/// socket, which only the owner at this machine can reach.
pub async fn run(
    plugins: &Plugins,
    workers: &WorkerSetup,
    plow: &Plow,
    local: bool,
    args: &Value,
) -> Done {
    let command = args["command"].as_str().unwrap_or_default();
    let mut audit = Map::new();
    audit.insert("command".into(), json!(command));
    if let Some(name) = args["name"].as_str() {
        audit.insert("name".into(), json!(name));
    }
    let result = match command {
        "list" => Ok(plugins.list()),
        "enable" | "disable" if !local => Err(owner_only()),
        "enable" => match Connector::named(args["name"].as_str().unwrap_or_default()) {
            Ok(connector) => enable(plugins, workers, plow, connector).await,
            Err(failure) => Err(failure),
        },
        "disable" => match Connector::named(args["name"].as_str().unwrap_or_default()) {
            Ok(connector) => disable(plugins, connector).await,
            Err(failure) => Err(failure),
        },
        _ => Err(Failure::usage(
            "bad_command",
            format!("unknown plugins command {command:?}"),
        )),
    };
    crate::vault::host::done(result, audit)
}

fn owner_only() -> Failure {
    Failure::new(
        Exit::Denied,
        "owner_only",
        "plugins are turned on and off by the owner, on the host itself".into(),
    )
}

/// Runs `connector`'s setup and enables it only if that succeeds. A setup
/// that fails leaves it `needs_setup`, with the failure as the reason.
async fn enable(
    plugins: &Plugins,
    workers: &WorkerSetup,
    plow: &Plow,
    connector: Connector,
) -> Result<Value, Failure> {
    let setup = match connector {
        Connector::Google => google(workers, plow).await,
        #[cfg(target_os = "macos")]
        Connector::Messages | Connector::Contacts => {
            full_disk_access(workers, &plugins.home, connector).await
        }
    };
    match setup {
        Ok(mut result) => {
            plugins.set(connector, Some(Setting::Enabled))?;
            result["plugin"] = json!(connector.name());
            result["state"] = json!("enabled");
            Ok(result)
        }
        Err(failure) => {
            let reason = failure.message.clone();
            plugins.set(connector, Some(Setting::NeedsSetup { reason }))?;
            Err(failure)
        }
    }
}

/// Turns `connector` off at once for new operations, then waits for the
/// ones already running (each bounded by its own time limit).
async fn disable(plugins: &Plugins, connector: Connector) -> Result<Value, Failure> {
    let running = plugins.set(connector, None)?;
    plugins.idle(connector).await;
    Ok(json!({"plugin": connector.name(), "state": "disabled", "waited_for": running}))
}

/// Google's setup: the pinned gog is installed, and Plow has a Google account
/// connected that it gives this host credentials for. Nothing is kept here:
/// Plow still hands out short-lived credentials per operation.
async fn google(workers: &WorkerSetup, plow: &Plow) -> Result<Value, Failure> {
    let gog = crate::google::host::gog_program(&workers.state_dir, &crate::google::PIN.version)?;
    // The check every Google worker makes before it runs gog.
    crate::google::process::verify(&gog)?;
    let executable = std::fs::metadata(&gog)
        .is_ok_and(|gog| gog.is_file() && gog.permissions().mode() & 0o111 != 0);
    if !executable {
        let message = format!("{} cannot be run; run install.sh again", gog.display());
        return Err(Failure::failed("gog_not_executable", message));
    }
    let tokens = plow.google_tokens().await?;
    let accounts: Vec<&str> = tokens
        .accounts
        .iter()
        .filter(|account| account.access_token.is_some())
        .map(|account| account.account.as_str())
        .collect();
    if accounts.is_empty() {
        let message = "Plow has no Google account this host can use; connect one in Plow, \
                       then run `latch plugins enable google` again";
        return Err(Failure::failed("google_not_connected", message.into()));
    }
    Ok(json!({"accounts": accounts}))
}

/// Messages' and Contacts' setup: Full Disk Access as this host sees it,
/// checked from a worker it starts. Not granted, it says what to grant and
/// where, and that the host must restart before the next check.
#[cfg(target_os = "macos")]
async fn full_disk_access(
    workers: &WorkerSetup,
    home: &Path,
    connector: Connector,
) -> Result<Value, Failure> {
    use crate::messages::access::{self, Status};
    let access = access::check(workers, connector, home).await;
    let mut details = json!({"access": access.status, "reason": access.reason});
    let message = match access.status {
        Status::Granted => return Ok(json!({"access": access.status})),
        Status::NotGranted => {
            let grant = grantee(&workers.program);
            details["grant"] = json!(grant);
            details["settings"] = json!(access::SETTINGS);
            format!(
                "this host has no Full Disk Access; turn it on for {} in System Settings \
                 ({}), restart the host, then run `{}` again",
                grant.display(),
                access::SETTINGS,
                connector.enable_command()
            )
        }
        Status::Unavailable => format!(
            "{} cannot be checked on this host: {}",
            connector.name(),
            access.reason
        ),
    };
    let mut failure = Failure::failed("needs_setup", message);
    failure.details = details;
    Err(failure)
}

/// What the owner grants Full Disk Access to: the app bundle the host's
/// latch is inside, or that latch itself.
#[cfg(target_os = "macos")]
pub(crate) fn grantee(program: &Path) -> &Path {
    program
        .ancestors()
        .find(|dir| dir.extension().is_some_and(|ext| ext == "app"))
        .unwrap_or(program)
}

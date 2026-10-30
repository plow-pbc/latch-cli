//! The `plugins` operation: `list` for any caller the host lets in (what
//! `latch status` asks each host);
//! `enable` and `disable` only from the owner at this host itself, over its
//! private socket. Neither is ever judged: the owner's commands on the host
//! are the owner's, and an agent gets `owner_only` for both.

#[cfg(target_os = "macos")]
use std::path::Path;

use serde_json::{Map, Value, json};

use super::{Connector, Plugins, Setting};
use crate::failure::{Failure, code};
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
            Ok(connector) => enable(plugins, workers, plow, connector, false).await,
            Err(failure) => Err(failure),
        },
        "disable" => match Connector::named(args["name"].as_str().unwrap_or_default()) {
            Ok(connector) => disable(plugins, connector).await,
            Err(failure) => Err(failure),
        },
        _ => Err(Failure::new(
            code::bad_command,
            format!("unknown plugins command {command:?}"),
        )),
    };
    crate::vault::host::done(result, audit)
}

fn owner_only() -> Failure {
    Failure::new(
        code::owner_only,
        "plugins are turned on and off by the owner, on the host itself".into(),
    )
}

/// Runs `connector`'s setup and enables it only if that succeeds. A setup
/// that fails leaves it `needs_setup`, with the failure as the reason. A
/// setup the owner turned the connector on or off during changes nothing.
/// With no host running, Full Disk Access is checked as the app instead.
pub(super) async fn enable(
    plugins: &Plugins,
    workers: &WorkerSetup,
    plow: &Plow,
    connector: Connector,
    #[cfg_attr(not(target_os = "macos"), expect(unused_variables))] no_host: bool,
) -> Result<Value, Failure> {
    let revision = plugins.revision(connector);
    let setup = match connector {
        Connector::Google => google(workers, plow).await,
        #[cfg(target_os = "macos")]
        Connector::Messages | Connector::Contacts => {
            use crate::messages::access;
            let access = if no_host {
                access::check_as_app(&workers.state_dir, connector).await
            } else {
                access::check(workers, connector, &plugins.home).await
            };
            let program = workers.program.canonicalize();
            let program = program.as_deref().unwrap_or(&workers.program);
            full_disk_access(access, program, connector, no_host)
        }
    };
    match setup {
        Ok(mut result) => {
            plugins.set(connector, Some(Setting::Enabled), Some(revision))?;
            result["plugin"] = json!(connector.name());
            result["state"] = json!("enabled");
            Ok(result)
        }
        Err(failure) => {
            let reason = failure.message.clone();
            plugins.set(
                connector,
                Some(Setting::NeedsSetup { reason }),
                Some(revision),
            )?;
            Err(failure)
        }
    }
}

/// Turns `connector` off at once for new operations, then waits for the
/// ones already running (each bounded by its own time limit).
pub(super) async fn disable(plugins: &Plugins, connector: Connector) -> Result<Value, Failure> {
    let running = plugins.set(connector, None, None)?;
    plugins.idle(connector).await;
    Ok(json!({"plugin": connector.name(), "state": "disabled", "waited_for": running}))
}

/// Whether a file's mode lets it run. Windows runs any file it can read.
fn runnable(meta: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    return std::os::unix::fs::PermissionsExt::mode(&meta.permissions()) & 0o111 != 0;
    #[cfg(windows)]
    return meta.is_file();
}

/// Google's setup: the pinned gog is installed, and Plow has a Google account
/// connected that it gives this host credentials for. Nothing is kept here:
/// Plow still hands out short-lived credentials per operation.
async fn google(workers: &WorkerSetup, plow: &Plow) -> Result<Value, Failure> {
    let gog = crate::google::host::gog_program(&workers.state_dir, &crate::google::PIN.version)?;
    // The check every Google worker makes before it runs gog.
    crate::google::process::verify(&gog)?;
    let executable = std::fs::metadata(&gog).is_ok_and(|gog| gog.is_file() && runnable(&gog));
    if !executable {
        let message = format!("{} cannot be run; run install.sh again", gog.display());
        return Err(Failure::new(code::gog_not_executable, message));
    }
    let tokens = match plow.google_tokens().await {
        Err(failure) if failure.code == code::google_not_connected.name => {
            return Err(google_not_connected(plow).await);
        }
        tokens => tokens?,
    };
    let accounts: Vec<&str> = tokens
        .accounts
        .iter()
        .filter(|account| account.access_token.is_some())
        .map(|account| account.account.as_str())
        .collect();
    let mut found = json!({"accounts": accounts});
    // The rest still work; these wait for the owner to sign in again.
    let needs_sign_in = tokens.needs_sign_in();
    if !needs_sign_in.is_empty() {
        found["needs_sign_in"] = json!(needs_sign_in);
    }
    Ok(found)
}

/// No Google account connected in Plow: where the owner connects one
/// (`connect_url`, also in the message), then the command to run again.
async fn google_not_connected(plow: &Plow) -> Failure {
    let page = plow.connections_page().await.ok();
    let place = match &page {
        Some(page) => format!("connect one at {page}"),
        None => "connect one in Plow".to_string(),
    };
    let message = format!(
        "Plow has no Google account this host can use; {place}, then run \
         `latch plugins enable google` again"
    );
    let mut failure = Failure::new(code::google_not_connected, message);
    failure.details = json!({"connect_url": page});
    failure
}

/// Messages' and Contacts' setup: Full Disk Access as the host sees it.
/// Not granted, it says what to grant and where, and, when a host runs,
/// that it must restart before the next check.
#[cfg(target_os = "macos")]
fn full_disk_access(
    access: crate::messages::access::Access,
    program: &Path,
    connector: Connector,
    no_host: bool,
) -> Result<Value, Failure> {
    use crate::messages::access::{self, Status};
    let mut details = json!({"access": access.status, "reason": access.reason});
    let message = match access.status {
        Status::Granted => return Ok(json!({"access": access.status})),
        Status::NotGranted => {
            let grant = grantee(program);
            details["grant"] = json!(grant);
            details["settings"] = json!(access::SETTINGS);
            format!(
                "this host has no Full Disk Access; turn it on for {} in System Settings \
                 ({}), {}then run `{}` again",
                grant.display(),
                access::SETTINGS,
                if no_host { "" } else { "restart the host, " },
                connector.enable_command()
            )
        }
        Status::Unavailable => format!(
            "{} cannot be checked on this host: {}",
            connector.name(),
            access.reason
        ),
    };
    let mut failure = Failure::new(code::needs_setup, message);
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

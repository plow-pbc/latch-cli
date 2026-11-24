//! Host-side discovery and lifecycle of the tab a session owns. The
//! terminal-browser CLI needs the owner's environment and control socket.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

use crate::failure::{Failure, code};

/// The engine's name, in commands and internally alike.
pub const ENGINE: &str = "terminal-browser";

/// Where the owner gets it.
pub const INSTALL: &str = "https://terminal-browser.sh";

/// The owner's `terminal-browser` program.
pub fn program() -> Result<PathBuf, Failure> {
    if let Some(named) = std::env::var_os("LATCH_TERMINAL_BROWSER") {
        let path = std::path::absolute(&named)
            .map_err(|err| failed(format!("$LATCH_TERMINAL_BROWSER: {err}")))?;
        return executable(&path).then_some(path).ok_or_else(not_installed);
    }
    std::env::split_paths(&crate::worker::owner_path())
        .map(|folder| folder.join(ENGINE))
        .find(|path| path.is_absolute() && executable(path))
        .ok_or_else(not_installed)
}

fn executable(path: &Path) -> bool {
    path.metadata()
        .is_ok_and(|info| info.is_file() && info.permissions().mode() & 0o111 != 0)
}

fn not_installed() -> Failure {
    let mut failure = Failure::new(
        code::engine_not_installed,
        format!(
            "terminal-browser is not installed on this host; it is optional, and latch does not \
             install it: the host's owner gets it from {INSTALL} (or names it in \
             $LATCH_TERMINAL_BROWSER)"
        ),
    );
    failure.details = json!({"engine": ENGINE, "setup": INSTALL});
    failure
}

/// The tab latch opened for one session in the owner's terminal-browser.
pub struct Tab {
    program: PathBuf,
    browser: String,
    /// The browser's DevTools port on 127.0.0.1.
    pub port: u16,
    /// The tab's DevTools target.
    pub target: String,
}

#[derive(Deserialize)]
struct Listing {
    browsers: Vec<Browser>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Browser {
    key: String,
    cdp_port: Option<u16>,
    #[serde(default)]
    tabs: Vec<ListedTab>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListedTab {
    id: u64,
    target_id: Option<String>,
}

/// Opens a blank tab for a session in the last browser `program` lists.
pub async fn open(program: &Path) -> Result<Tab, Failure> {
    let listed = list(program).await?;
    let Some(browser) = listed.browsers.last() else {
        let mut failure = Failure::new(
            code::engine_not_running,
            "terminal-browser is not running on this host: the host's owner opens it in a \
             terminal pane (`terminal-browser open`), then the session can start"
                .into(),
        );
        failure.details = json!({"setup": "terminal-browser open"});
        return Err(failure);
    };
    let key = &browser.key;
    let port = browser
        .cdp_port
        .filter(|port| *port != 0)
        .ok_or_else(|| failed("terminal-browser has no DevTools port".into()))?;
    let said = run(program, &["new-tab", "--browser", key, "about:blank"]).await?;
    let id = opened_tab(&said, key)
        .filter(|id| !browser.tabs.iter().any(|tab| tab.id == *id))
        .ok_or_else(|| failed("terminal-browser did not report a newly opened tab".into()))?;
    let target = list(program)
        .await?
        .browsers
        .into_iter()
        .find(|browser| browser.key == *key && browser.cdp_port == Some(port))
        .and_then(|browser| browser.tabs.into_iter().find(|tab| tab.id == id))
        .and_then(|tab| tab.target_id)
        .filter(|target| !target.is_empty() && !listed.browsers.iter().flat_map(|pane| &pane.tabs)
            .any(|tab| tab.target_id.as_ref() == Some(target)))
        .ok_or_else(|| failed(format!("terminal-browser opened tab {id}, but its CDP target is unavailable; close it in the pane")))?;
    Ok(Tab {
        program: program.to_path_buf(),
        browser: key.clone(),
        port,
        target,
    })
}

impl Tab {
    /// Close the tab by target identity, even if another tab is active.
    pub async fn close(&self) -> Result<(), Failure> {
        let args = [
            "action",
            "--browser",
            &self.browser,
            "--target",
            &self.target,
            "--",
            "tab",
            "close",
        ];
        run(&self.program, &args).await.map(drop)
    }
}

async fn list(program: &Path) -> Result<Listing, Failure> {
    let out = run(program, &["ls", "--all", "--json"]).await?;
    serde_json::from_str(&out).map_err(|err| failed(format!("ls --json: {err}")))
}

/// The CLI reports `opened tab ID in KEY (terminal-browser ls for details)`.
fn opened_tab(said: &str, key: &str) -> Option<u64> {
    let (id, browser) = said
        .trim()
        .strip_prefix("opened tab ")?
        .split_once(" in ")?;
    (browser.split_whitespace().next()? == key)
        .then(|| id.parse().ok())
        .flatten()
}

async fn run(program: &Path, args: &[&str]) -> Result<String, Failure> {
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .env("PATH", crate::worker::owner_path())
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let command_name = if args[0] == "action" {
        "tab close"
    } else {
        args[0]
    };
    let out = tokio::time::timeout(Duration::from_secs(5), command.output())
        .await
        .map_err(|_| failed(format!("terminal-browser {command_name} timed out")))?
        .map_err(|err| failed(format!("terminal-browser {command_name}: {err}")))?;
    if !out.status.success() {
        // The CLI can print the owner's other tabs' URLs in its errors.
        return Err(failed(format!(
            "terminal-browser {command_name} failed ({})",
            out.status
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn failed(message: String) -> Failure {
    Failure::new(code::engine_failed, message)
}

#[cfg(test)]
mod tests;

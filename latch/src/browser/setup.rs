//! `latch browser setup`: the owner installs a browser engine on the host
//! after the fact (`install.sh --no-browser`, say). It runs the same
//! `browser-engines.sh` install.sh runs, from a copy carried in latch with
//! the pins it reads, so both download and check alike; then latch checks
//! the installed build as it does before every launch.
//!
//! It runs on this machine's own state directory, never over a connection,
//! and refuses a state directory logged in as an agent: an agent's browser
//! call never downloads an engine.

use std::process::Command;

use serde_json::{Value, json};

use super::engine::{CAMOUFOX, CFT};
use crate::cli::EngineArg;
use crate::failure::{Exit, Failure};
use crate::state::StateDir;

/// What `browser-engines.sh` reads, at the paths it reads them from.
const FILES: [(&str, &[u8]); 8] = [
    (
        "browser-engines.sh",
        include_bytes!("../../browser-engines.sh"),
    ),
    (
        "install-packages.sh",
        include_bytes!("../../install-packages.sh"),
    ),
    ("src/browser/cft.json", include_bytes!("cft.json")),
    ("src/browser/camoufox.json", include_bytes!("camoufox.json")),
    (
        "camoufox/package.json",
        include_bytes!("../../camoufox/package.json"),
    ),
    (
        "camoufox/package-lock.json",
        include_bytes!("../../camoufox/package-lock.json"),
    ),
    (
        "camoufox/sqlite/package.json",
        include_bytes!("../../camoufox/sqlite/package.json"),
    ),
    (
        "camoufox/sqlite/index.js",
        include_bytes!("../../camoufox/sqlite/index.js"),
    ),
];

pub fn run(state: &StateDir, engine: EngineArg) -> Result<Value, Failure> {
    if let Some(config) = state.load_config()?
        && config.peer.kind == "agent"
    {
        return Err(Failure::new(
            Exit::Denied,
            "owner_only",
            "only the host's owner installs a browser engine, on the host: this latch is \
             logged in as an agent"
                .to_string(),
        ));
    }
    let name = match engine {
        EngineArg::Cft => "chrome",
        EngineArg::Camoufox => "camoufox",
    };
    install(state, name)?;
    let (version, folder) = match engine {
        EngineArg::Cft => {
            CFT.verified(&state.path)?;
            (&CFT.version, CFT.folder(&state.path))
        }
        EngineArg::Camoufox => {
            CAMOUFOX.verified(&state.path)?;
            (&CAMOUFOX.version, CAMOUFOX.folder(&state.path))
        }
    };
    Ok(json!({"engine": name, "version": version, "folder": folder}))
}

/// Runs the carried `browser-engines.sh` for `engine` into `state`. Its
/// messages go to trace.log, so stdout keeps only latch's JSON and stderr
/// only its error line.
fn install(state: &StateDir, engine: &str) -> Result<(), Failure> {
    let local = |err: std::io::Error| Failure::failed("setup_failed", err.to_string());
    let files = tempfile::tempdir().map_err(local)?;
    for (path, bytes) in FILES {
        let path = files.path().join(path);
        std::fs::create_dir_all(path.parent().expect("a carried file is in a folder"))
            .map_err(local)?;
        std::fs::write(&path, bytes).map_err(local)?;
    }
    let (out, err) = crate::trace::output(&crate::trace::file(state)?)?;
    let status = Command::new("/bin/sh")
        .arg(files.path().join("browser-engines.sh"))
        .arg("--state-dir")
        .arg(&state.path)
        .arg(engine)
        .stdout(out)
        .stderr(err)
        .status()
        .map_err(local)?;
    if !status.success() {
        return Err(Failure::failed(
            "setup_failed",
            format!(
                "installing {engine} failed ({status}); its messages are in {}",
                state.path.join(crate::trace::FILE).display()
            ),
        ));
    }
    Ok(())
}

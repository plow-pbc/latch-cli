//! `latch browser setup`: the owner installs a browser engine on the host
//! after the fact (`install.sh --no-browser`, say). It runs the same
//! `browser-engines.sh` install.sh runs, from a copy carried in latch with
//! the pins it reads, so both download and check alike; then latch checks
//! the installed build as it does before every launch.
//!
//! It runs on this machine's own state directory, never over a connection,
//! and refuses a state directory logged in as an agent: an agent's browser
//! call never downloads an engine.
//!
//! Windows has no `sh`: there latch installs Chrome for Testing itself, as
//! `browser-engines.sh` does elsewhere, with the system's curl.exe and
//! tar.exe. Camoufox has no Windows build.

#[cfg(unix)]
use std::process::Command;

use serde_json::{Value, json};

use super::engine::{CAMOUFOX, CFT};
use crate::cli::EngineArg;
use crate::failure::{Failure, code};
use crate::state::StateDir;

/// What `browser-engines.sh` reads, at the paths it reads them from.
#[cfg(unix)]
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
            code::owner_only,
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
#[cfg(unix)]
fn install(state: &StateDir, engine: &str) -> Result<(), Failure> {
    let local = |err: std::io::Error| Failure::new(code::setup_failed, err.to_string());
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
        return Err(Failure::new(
            code::setup_failed,
            format!(
                "installing {engine} failed ({status}); its messages are in {}",
                state.path.join(crate::trace::FILE).display()
            ),
        ));
    }
    Ok(())
}

/// The capability Chrome's network service runs with on Windows (Chrome's
/// own sandbox there is a less privileged AppContainer), as
/// `DeriveCapabilitySidsFromName("lpacChromeInstallFiles")` gives it.
/// Chrome's installer lets it read the build; without that, Chrome cannot
/// start the service in its sandbox and runs it without one.
#[cfg(windows)]
const CHROME_INSTALL_FILES: &str = "S-1-15-3-1024-2302894289-466761758-1166120688-1039016420-2430351297-4240214049-4028510897-3317428798";

/// Downloads this machine's Chrome for Testing build with curl.exe, checks
/// the zip against the pin, and unpacks it with tar.exe into a folder
/// Chrome's sandbox may read, which then becomes the version folder. A
/// build already there that checks out is kept.
#[cfg(windows)]
fn install(state: &StateDir, engine: &str) -> Result<(), Failure> {
    use std::ffi::OsStr;
    use std::fs;

    let failed = |message: String| Failure::new(code::setup_failed, message);
    let local = |err: std::io::Error| failed(err.to_string());
    if engine != "chrome" {
        return Err(Failure::new(
            code::unsupported_platform,
            format!("no {engine} build for Windows"),
        ));
    }
    let Some(build) = super::engine::platform().and_then(|key| CFT.builds.get(key)) else {
        return Err(Failure::new(
            code::unsupported_platform,
            format!("no Chrome for Testing build for {}", std::env::consts::ARCH),
        ));
    };
    if CFT.verified(&state.path).is_ok() {
        return Ok(());
    }
    // Staged in the version folder, as browser-engines.sh does: a host
    // starting meanwhile removes every other folder beside it.
    let version = CFT.folder(&state.path);
    fs::create_dir_all(&version).map_err(local)?;
    let staging = tempfile::Builder::new()
        .prefix(".download.")
        .tempdir_in(&version)
        .map_err(local)?;
    let run = |program: &str, args: &[&OsStr]| crate::trace::system_tool(state, program, args);
    let zip = staging.path().join("cft.zip");
    run(
        "curl.exe",
        &[
            "-fsS".as_ref(),
            "-o".as_ref(),
            zip.as_os_str(),
            build.url.as_ref(),
        ],
    )?;
    let sha256 =
        crate::google::process::sha256(&mut fs::File::open(&zip).map_err(local)?).map_err(local)?;
    if sha256 != build.zip_sha256 {
        return Err(failed(format!(
            "{}: sha256 {sha256} is not the pinned {}; nothing installed",
            build.url, build.zip_sha256
        )));
    }
    // Granted before the build is in it, so every file it unpacks inherits it.
    let unpacked = staging.path().join("x");
    fs::create_dir(&unpacked).map_err(local)?;
    let grant = format!("*{CHROME_INSTALL_FILES}:(OI)(CI)(RX)");
    run(
        "icacls.exe",
        &[
            unpacked.as_os_str(),
            "/grant".as_ref(),
            grant.as_ref(),
            "/Q".as_ref(),
        ],
    )?;
    run(
        "tar.exe",
        &[
            "-xf".as_ref(),
            zip.as_os_str(),
            "-C".as_ref(),
            unpacked.as_os_str(),
        ],
    )?;
    let folder = version.join(&build.folder);
    match fs::remove_dir_all(&folder) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(local(err)),
    }
    fs::rename(unpacked.join(&build.folder), &folder).map_err(local)?;
    fs::write(version.join("manifest.json"), include_bytes!("cft.json")).map_err(local)
}

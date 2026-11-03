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
//! Windows has no `sh`: there latch installs either engine itself, as
//! `browser-engines.sh` does elsewhere, with the system's curl.exe and
//! tar.exe, and Camoufox's runtime with the pinned Node's npm.

#[cfg(windows)]
use std::ffi::OsStr;
#[cfg(windows)]
use std::fs;
#[cfg(windows)]
use std::path::{Path, PathBuf};
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

/// Installs `engine` as `browser-engines.sh` does elsewhere: each archive
/// downloaded with curl.exe and checked against the pin, unpacked with
/// tar.exe, in a staging folder inside the version folder (a host starting
/// meanwhile removes every other folder beside it), and the manifest put
/// in last. A build already there that checks out is kept.
#[cfg(windows)]
fn install(state: &StateDir, engine: &str) -> Result<(), Failure> {
    let failed = |message: String| Failure::new(code::setup_failed, message);
    let local = |err: std::io::Error| failed(err.to_string());
    let Some(key) = super::engine::platform() else {
        return Err(Failure::new(
            code::unsupported_platform,
            format!("no {engine} build for {}", std::env::consts::ARCH),
        ));
    };
    let version = match engine {
        "chrome" if CFT.verified(&state.path).is_ok() => return Ok(()),
        "chrome" => CFT.folder(&state.path),
        _ if CAMOUFOX.verified(&state.path).is_ok() => return Ok(()),
        _ => CAMOUFOX.folder(&state.path),
    };
    fs::create_dir_all(&version).map_err(local)?;
    let staging = tempfile::Builder::new()
        .prefix(".download.")
        .tempdir_in(&version)
        .map_err(local)?;
    let staging = staging.path();
    let run = |program: &str, args: &[&OsStr]| crate::trace::system_tool(state, program, args);
    // Downloads `url` to `to` and checks it is the pinned `sha256`.
    let fetch = |url: &str, sha256: &str, to: &Path| -> Result<(), Failure> {
        // Following redirects (GitHub's releases have one), only to https.
        run(
            "curl",
            &[
                "-fsSL".as_ref(),
                "--proto".as_ref(),
                "=https".as_ref(),
                "-o".as_ref(),
                to.as_os_str(),
                url.as_ref(),
            ],
        )?;
        let got = crate::google::process::sha256(&mut fs::File::open(to).map_err(local)?)
            .map_err(local)?;
        match got == sha256 {
            true => Ok(()),
            false => Err(failed(format!(
                "{url}: sha256 {got} is not the pinned {sha256}; nothing installed"
            ))),
        }
    };
    let unpack = |archive: &Path, into: &Path| {
        run(
            "tar",
            &[
                "-xf".as_ref(),
                archive.as_os_str(),
                "-C".as_ref(),
                into.as_os_str(),
            ],
        )
    };
    // Each part, unpacked in `staging`, and the folder it becomes.
    let parts: Vec<(PathBuf, PathBuf)> = match engine {
        "chrome" => {
            let build = &CFT.builds[key];
            let zip = staging.join("cft.zip");
            fetch(&build.url, &build.zip_sha256, &zip)?;
            // Granted before the build is in it, so every file it unpacks
            // inherits it.
            let unpacked = staging.join("x");
            fs::create_dir(&unpacked).map_err(local)?;
            let grant = format!("*{CHROME_INSTALL_FILES}:(OI)(CI)(RX)");
            run(
                "icacls",
                &[
                    unpacked.as_os_str(),
                    "/grant".as_ref(),
                    grant.as_ref(),
                    "/Q".as_ref(),
                ],
            )?;
            unpack(&zip, &unpacked)?;
            vec![(unpacked.join(&build.folder), version.join(&build.folder))]
        }
        _ => {
            let build = &CAMOUFOX.builds[key];
            let fox = staging.join("camoufox");
            fs::create_dir(&fox).map_err(local)?;
            let zip = staging.join("camoufox.zip");
            fetch(&build.camoufox.url, &build.camoufox.sha256, &zip)?;
            unpack(&zip, &fox)?;
            // What camoufox-js reads to know the build, as browser-engines.sh
            // writes it.
            let (number, label) = CAMOUFOX.version.split_once('-').unwrap_or_default();
            fs::write(
                fox.join("version.json"),
                format!(r#"{{"version":"{number}","build":"{label}"}}"#),
            )
            .map_err(local)?;
            let zip = staging.join("node.zip");
            fetch(&build.node.url, &build.node.sha256, &zip)?;
            unpack(&zip, staging)?;
            let node = staging.join("node");
            fs::rename(staging.join(&build.node.folder), &node).map_err(local)?;
            let runtime = staging.join("runtime");
            for (path, bytes) in RUNTIME {
                let path = runtime.join(path);
                fs::create_dir_all(path.parent().expect("in a folder")).map_err(local)?;
                fs::write(&path, bytes).map_err(local)?;
            }
            npm_ci(state, &node, &runtime, staging)?;
            ["camoufox", "node", "runtime"]
                .into_iter()
                .map(|part| (staging.join(part), version.join(part)))
                .collect()
        }
    };
    for (unpacked, folder) in parts {
        match fs::remove_dir_all(&folder) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(local(err)),
        }
        fs::rename(unpacked, &folder).map_err(local)?;
    }
    let manifest: &[u8] = match engine {
        "chrome" => include_bytes!("cft.json"),
        _ => include_bytes!("camoufox.json"),
    };
    fs::write(version.join("manifest.json"), manifest).map_err(local)
}

/// The Camoufox helper's package and lockfile, which `npm ci` installs.
#[cfg(windows)]
const RUNTIME: [(&str, &[u8]); 4] = [
    (
        "package.json",
        include_bytes!("../../camoufox/package.json"),
    ),
    (
        "package-lock.json",
        include_bytes!("../../camoufox/package-lock.json"),
    ),
    (
        "sqlite/package.json",
        include_bytes!("../../camoufox/sqlite/package.json"),
    ),
    (
        "sqlite/index.js",
        include_bytes!("../../camoufox/sqlite/index.js"),
    ),
];

/// Installs the helper's runtime in `runtime` from its lockfile, with the
/// pinned `node`'s own npm, as browser-engines.sh does: integrity-checked
/// by npm, no package's install script run, its cache and home in
/// `scratch`. The local package (`sqlite`) is copied in rather than linked:
/// npm's link on Windows is a junction to where it was installed, which
/// the move into the version folder leaves dangling.
#[cfg(windows)]
fn npm_ci(state: &StateDir, node: &Path, runtime: &Path, scratch: &Path) -> Result<(), Failure> {
    let system = PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_default());
    let mut path = std::ffi::OsString::from(node);
    path.push(";");
    path.push(system.join("System32"));
    let (out, err) = crate::trace::output(&crate::trace::file(state)?)?;
    let mut command = Command::new(node.join("node.exe"));
    command
        .arg(node.join("node_modules/npm/bin/npm-cli.js"))
        .args([
            "ci",
            "--ignore-scripts",
            "--omit=dev",
            "--no-audit",
            "--no-fund",
            "--install-links",
        ])
        .current_dir(runtime)
        .env_clear()
        .env("PATH", path)
        .env("SystemRoot", &system)
        .env("npm_config_cache", scratch.join("npm-cache"))
        .env("npm_config_update_notifier", "false");
    for name in ["HOME", "USERPROFILE", "APPDATA", "LOCALAPPDATA"] {
        command.env(name, scratch);
    }
    let status = command
        .stdout(out)
        .stderr(err)
        .status()
        .map_err(|err| Failure::new(code::setup_failed, format!("npm: {err}")))?;
    match status.success() {
        true => Ok(()),
        false => Err(Failure::new(
            code::setup_failed,
            format!(
                "npm ci of the Camoufox runtime failed ({status}); its messages are in {}",
                state.path.join(crate::trace::FILE).display()
            ),
        )),
    }
}

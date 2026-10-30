//! On Windows, which has no install.sh, latch installs the pinned gog
//! itself, as install.sh does elsewhere: `plugins enable google` when it is
//! missing, and the installer (`latch install-gog`).

use std::fs;

use serde_json::{Value, json};

use super::{GOG, PIN};
use crate::failure::{Failure, code};
use crate::state::StateDir;

/// Where install.sh fetches gog's releases from too.
const RELEASES: &str = "https://github.com/openclaw/gogcli/releases/download";

/// Installs the pinned gog in `state`'s plugins folder unless it is there
/// and checks out: the release's Windows zip with curl.exe, its gog.exe
/// unpacked with tar.exe and checked against the pin before it is put in
/// place. It is staged in the version folder, as a host starting removes
/// everything beside it.
pub fn if_missing(state: &StateDir) -> Result<Value, Failure> {
    let program = super::host::gog_path(&state.path, &PIN.version);
    let answer =
        |installed| json!({"gog": program, "version": PIN.version, "installed": installed});
    if super::process::verify(&program).is_ok() {
        return Ok(answer(false));
    }
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => {
            return Err(Failure::new(
                code::unsupported_platform,
                format!("no gog build for Windows on {other}"),
            ));
        }
    };
    let folder = program.parent().expect("gog is in its version folder");
    let failed = |err: std::io::Error| {
        Failure::new(code::setup_failed, format!("{}: {err}", folder.display()))
    };
    fs::create_dir_all(folder).map_err(failed)?;
    let staging = tempfile::Builder::new()
        .prefix(".download.")
        .tempdir_in(folder)
        .map_err(failed)?;
    let zip = staging.path().join("gog.zip");
    let url = format!(
        "{RELEASES}/v{version}/gogcli_{version}_windows_{arch}.zip",
        version = PIN.version
    );
    let run =
        |program: &str, args: &[&std::ffi::OsStr]| crate::trace::system_tool(state, program, args);
    run(
        "curl.exe",
        &[
            "-fsSL".as_ref(),
            "-o".as_ref(),
            zip.as_os_str(),
            url.as_ref(),
        ],
    )?;
    run(
        "tar.exe",
        &[
            "-xf".as_ref(),
            zip.as_os_str(),
            "-C".as_ref(),
            staging.path().as_os_str(),
            GOG.as_ref(),
        ],
    )?;
    let unpacked = staging.path().join(GOG);
    super::process::verify(&unpacked)?;
    fs::rename(&unpacked, &program).map_err(failed)?;
    Ok(answer(true))
}

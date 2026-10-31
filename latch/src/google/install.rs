//! The pinned gog, installed in a state folder's plugins folder: `latch
//! install-gog`, on every system. install.sh downloads it the same way on
//! macOS and Linux; on Windows the installer and `latch update` run this.

use std::fs;

use serde_json::{Value, json};

use super::{GOG, PIN};
use crate::failure::{Failure, code};
use crate::state::StateDir;

/// Where install.sh fetches gog's releases from too.
const RELEASES: &str = "https://github.com/openclaw/gogcli/releases/download";

/// Installs the pinned gog in `state`'s plugins folder unless it is there
/// and checks out: this system's release archive with curl, its gog
/// unpacked with tar and checked against the pin before it is put in
/// place. It is staged in the version folder, as a host starting removes
/// everything beside it.
pub fn if_missing(state: &StateDir) -> Result<Value, Failure> {
    let program = super::host::gog_path(&state.path, &PIN.version);
    let answer =
        |installed| json!({"gog": program, "version": PIN.version, "installed": installed});
    if super::process::verify(&program).is_ok() {
        return Ok(answer(false));
    }
    let unsupported = |what: &str| {
        Failure::new(
            code::unsupported_platform,
            format!("no gog release for {what}"),
        )
    };
    // The archive's name for this system, its kind, and gog's name in it.
    let (os, archive, member) = match std::env::consts::OS {
        "macos" => ("darwin", "tar.gz", "./gog"),
        "linux" => ("linux", "tar.gz", "./gog"),
        "windows" => ("windows", "zip", "gog.exe"),
        other => return Err(unsupported(other)),
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => return Err(unsupported(other)),
    };
    let folder = program.parent().expect("gog is in its version folder");
    let failed = |err: std::io::Error| {
        Failure::new(code::setup_failed, format!("{}: {err}", folder.display()))
    };
    state.create()?;
    fs::create_dir_all(folder).map_err(failed)?;
    let staging = tempfile::Builder::new()
        .prefix(".download.")
        .tempdir_in(folder)
        .map_err(failed)?;
    let download = staging.path().join(format!("gog.{archive}"));
    let url = format!(
        "{RELEASES}/v{version}/gogcli_{version}_{os}_{arch}.{archive}",
        version = PIN.version
    );
    let run =
        |program: &str, args: &[&std::ffi::OsStr]| crate::trace::system_tool(state, program, args);
    run(
        "curl",
        &[
            "-fsSL".as_ref(),
            "--proto".as_ref(),
            "=https".as_ref(),
            "-o".as_ref(),
            download.as_os_str(),
            url.as_ref(),
        ],
    )?;
    run(
        "tar",
        &[
            "-xf".as_ref(),
            download.as_os_str(),
            "-C".as_ref(),
            staging.path().as_os_str(),
            member.as_ref(),
        ],
    )?;
    let unpacked = staging.path().join(GOG);
    // A file, never a link the archive made.
    if !fs::symlink_metadata(&unpacked).map_err(failed)?.is_file() {
        return Err(Failure::new(
            code::setup_failed,
            format!("{url} has no gog file in it; nothing installed"),
        ));
    }
    super::process::verify(&unpacked)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&unpacked, fs::Permissions::from_mode(0o755)).map_err(failed)?;
    }
    fs::rename(&unpacked, &program).map_err(failed)?;
    Ok(answer(true))
}

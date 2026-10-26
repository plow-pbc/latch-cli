//! `latch app`: opens Plow Latch.app, the Mac app this latch belongs to.
//! On macOS a release's latch is the app's own executable
//! (`Plow Latch.app/Contents/MacOS/latch`), wherever the app is, and
//! `~/.local/bin/latch` links to it: the bundle is found from that, never
//! from a fixed folder. A state folder goes to the app as the installer
//! passes it (`open APP --args --state-dir ABS`), absolute, since the app
//! runs from another folder.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::failure::{Failure, code};

/// What `latch app` prints: the app it opened, and the state folder it
/// handed it, if any.
#[derive(Serialize, Debug, PartialEq)]
pub struct Opened {
    pub app: PathBuf,
    pub state_dir: Option<PathBuf>,
}

/// Opens this latch's app, on `state_dir` when one was given (or
/// `LATCH_STATE_DIR` names one).
pub fn open(state_dir: Option<PathBuf>) -> Result<Opened, Failure> {
    let exe = std::env::current_exe()
        .map_err(|err| Failure::new(code::io_error, format!("can't find this latch: {err}")))?;
    let (opened, args) = opening(std::env::consts::OS, &exe, state_dir)?;
    let output = std::process::Command::new("open")
        .args(&args)
        .output()
        .map_err(|err| could_not_open(&opened.app, &err.to_string()))?;
    if !output.status.success() {
        let said = String::from_utf8_lossy(&output.stderr);
        return Err(could_not_open(&opened.app, said.trim()));
    }
    Ok(opened)
}

/// The app `exe` belongs to on `os`, and the arguments `open` gets.
fn opening(
    os: &str,
    exe: &Path,
    state_dir: Option<PathBuf>,
) -> Result<(Opened, Vec<OsString>), Failure> {
    if os != "macos" {
        let mut failure = Failure::new(
            code::unsupported_platform,
            format!("Plow Latch.app is a Mac app; on {os}, latch runs from the terminal"),
        );
        failure.next = Some("latch setup".into());
        return Err(failure);
    }
    let Some(app) = bundle_of(exe) else {
        let message = format!(
            "this latch ({}) is not inside Plow Latch.app, so it has no app to open; \
             install a release with install.sh, or run the latch inside the app",
            exe.display()
        );
        let mut failure = Failure::new(code::local_build, message);
        failure.next = Some("Plow Latch.app/Contents/MacOS/latch app".into());
        return Err(failure);
    };
    let state_dir = state_dir.map(|dir| std::path::absolute(&dir).unwrap_or(dir));
    let mut args = vec![app.clone().into_os_string()];
    if let Some(dir) = &state_dir {
        args.extend([
            "--args".into(),
            "--state-dir".into(),
            dir.clone().into_os_string(),
        ]);
    }
    Ok((Opened { app, state_dir }, args))
}

/// The `.app` whose `Contents/MacOS` holds `exe`, after following links
/// (`~/.local/bin/latch` is one); `None` for a latch outside any app.
fn bundle_of(exe: &Path) -> Option<PathBuf> {
    let exe = std::fs::canonicalize(exe).unwrap_or_else(|_| exe.to_path_buf());
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let app = contents.parent()?;
    let is_app = app.extension().is_some_and(|ext| ext == "app");
    (macos.file_name()? == "MacOS" && contents.file_name()? == "Contents" && is_app)
        .then(|| app.to_path_buf())
}

fn could_not_open(app: &Path, why: &str) -> Failure {
    let mut failure = Failure::new(
        code::app_broken,
        format!("could not open {}: {why}", app.display()),
    );
    failure.next = Some("reinstall Latch".into());
    failure
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bundle laid out as a release's, in a temporary folder.
    fn bundle(root: &Path) -> PathBuf {
        let macos = root.join("Apps/Plow Latch.app/Contents/MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        let exe = macos.join("latch");
        std::fs::write(&exe, b"").unwrap();
        exe
    }

    #[test]
    fn the_app_is_the_bundle_around_this_latch() {
        let dir = tempfile::tempdir().unwrap();
        let exe = bundle(dir.path());
        let app = std::fs::canonicalize(dir.path().join("Apps/Plow Latch.app")).unwrap();
        assert_eq!(bundle_of(&exe), Some(app));
    }

    #[cfg(unix)]
    #[test]
    fn a_link_into_the_app_leads_to_it() {
        let dir = tempfile::tempdir().unwrap();
        let exe = bundle(dir.path());
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        std::os::unix::fs::symlink(&exe, bin.join("latch")).unwrap();
        let app = std::fs::canonicalize(dir.path().join("Apps/Plow Latch.app")).unwrap();
        assert_eq!(bundle_of(&bin.join("latch")), Some(app));
    }

    #[test]
    fn a_latch_outside_an_app_has_none() {
        let dir = tempfile::tempdir().unwrap();
        for path in [
            "target/debug/latch",
            "Contents/MacOS/latch",
            "x.app/MacOS/latch",
        ] {
            let exe = dir.path().join(path);
            std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
            std::fs::write(&exe, b"").unwrap();
            assert_eq!(bundle_of(&exe), None, "{path}");
        }
        let failure = opening("macos", &dir.path().join("target/debug/latch"), None).unwrap_err();
        assert_eq!(failure.code, "local_build");
    }

    #[test]
    fn the_state_folder_goes_to_the_app_absolute() {
        let dir = tempfile::tempdir().unwrap();
        let exe = bundle(dir.path());
        let (opened, args) = opening("macos", &exe, Some("dev/state".into())).unwrap();
        let state = std::env::current_dir().unwrap().join("dev/state");
        assert_eq!(opened.state_dir.as_deref(), Some(state.as_path()));
        let args: Vec<_> = args.iter().map(|arg| arg.to_str().unwrap()).collect();
        assert_eq!(
            args,
            [
                opened.app.to_str().unwrap(),
                "--args",
                "--state-dir",
                state.to_str().unwrap()
            ]
        );
        let (_, args) = opening("macos", &exe, None).unwrap();
        assert_eq!(args, [opened.app.into_os_string()]);
    }

    #[test]
    fn other_systems_have_no_app() {
        for os in ["linux", "windows"] {
            let failure = opening(os, Path::new("/usr/local/bin/latch"), None).unwrap_err();
            assert_eq!(failure.code, "unsupported_platform");
            assert_eq!(failure.next.as_deref(), Some("latch setup"));
        }
    }
}

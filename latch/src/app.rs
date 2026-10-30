//! `latch app`: opens Plow Latch.app, the Mac app this latch belongs to.
//! On macOS a release's latch is the app's own executable
//! (`Plow Latch.app/Contents/MacOS/latch`), wherever the app is, and
//! `~/.local/bin/latch` links to it: the bundle is found from that, never
//! from a fixed folder. The app picks its state folder itself, so `latch
//! app` hands it none and refuses one it is given.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::failure::{Failure, code};

/// What `latch app` prints: the app it opened.
#[derive(Serialize, Debug, PartialEq)]
pub struct Opened {
    pub app: PathBuf,
}

/// Opens this latch's app. `state_dir` is a folder `--state-dir` or
/// `LATCH_STATE_DIR` names, which the app does not take from here.
pub fn open(state_dir: Option<&Path>) -> Result<Opened, Failure> {
    let exe = std::env::current_exe()
        .map_err(|err| Failure::new(code::io_error, format!("can't find this latch: {err}")))?;
    let app = app_of(std::env::consts::OS, &exe, state_dir)?;
    let output = std::process::Command::new("open")
        .arg(&app)
        .output()
        .map_err(|err| could_not_open(&app, &err.to_string()))?;
    if !output.status.success() {
        let said = String::from_utf8_lossy(&output.stderr);
        return Err(could_not_open(&app, said.trim()));
    }
    Ok(Opened { app })
}

/// The app `exe` belongs to on `os`, if it can be opened as asked.
fn app_of(os: &str, exe: &Path, state_dir: Option<&Path>) -> Result<PathBuf, Failure> {
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
    if let Some(dir) = state_dir {
        let message = format!(
            "latch app opens the app on the state folder it picks itself, so it takes \
             no --state-dir or LATCH_STATE_DIR (given {})",
            dir.display()
        );
        let mut failure = Failure::new(code::bad_arguments, message);
        failure.next = Some("latch app, with no --state-dir and LATCH_STATE_DIR unset".into());
        return Err(failure);
    }
    Ok(app)
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

    /// The bundle around this latch, run directly or through a link as
    /// `~/.local/bin/latch` is.
    #[cfg(unix)]
    #[test]
    fn the_app_is_the_bundle_around_this_latch() {
        let dir = tempfile::tempdir().unwrap();
        let exe = bundle(dir.path());
        let link = dir.path().join("latch");
        std::os::unix::fs::symlink(&exe, &link).unwrap();
        let app = std::fs::canonicalize(dir.path().join("Apps/Plow Latch.app")).unwrap();
        for path in [&exe, &link] {
            assert_eq!(bundle_of(path).as_ref(), Some(&app), "{}", path.display());
            assert_eq!(app_of("macos", path, None).unwrap(), app);
        }
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
        let failure = app_of("macos", &dir.path().join("target/debug/latch"), None).unwrap_err();
        assert_eq!(failure.code, "local_build");
    }

    #[test]
    fn a_state_folder_is_refused_not_passed_on() {
        let dir = tempfile::tempdir().unwrap();
        let exe = bundle(dir.path());
        let failure = app_of("macos", &exe, Some(Path::new("/Users/me/dev"))).unwrap_err();
        assert_eq!(failure.code, "bad_arguments");
        assert!(
            failure.message.contains("/Users/me/dev"),
            "{}",
            failure.message
        );
    }

    #[test]
    fn other_systems_have_no_app() {
        for os in ["linux", "windows"] {
            let failure = app_of(os, Path::new("/usr/local/bin/latch"), None).unwrap_err();
            assert_eq!(failure.code, "unsupported_platform");
            assert_eq!(failure.next.as_deref(), Some("latch setup"));
        }
    }
}

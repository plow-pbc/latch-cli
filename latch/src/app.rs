//! `latch app`: opens Plow Latch.app, the Mac app this latch belongs to.
//! On macOS a release's latch is the app's own executable
//! (`Plow Latch.app/Contents/MacOS/latch`), wherever the app is, and
//! `~/.local/bin/latch` links to it: the bundle is found from that, never
//! from a fixed folder. A state folder goes to the app as the installer
//! passes it (`open APP --args --state-dir ABS`), absolute, since the app
//! runs from another folder.
//!
//! `open` hands arguments only to an app it starts: one already open just
//! comes to the front, on the folder it has. The app shows one state
//! folder and runs one copy, so a `latch app` naming another folder than
//! the open app's is refused rather than said to have opened it.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::failure::{Failure, code};
use crate::state::StateDir;

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
    if let Some(wanted) = &opened.state_dir {
        let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
        let default = StateDir::new(crate::cli::BUILD_STATE_DIR.map(PathBuf::from))?.path;
        if let Some(other) = open_elsewhere(&running(&exe), wanted, &default) {
            return Err(open_on(&opened.app, &other, wanted));
        }
    }
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

/// The state folders of this app's open copies, as `ps` lists them: a
/// LaunchServices launch runs `exe` itself, with no arguments or exactly
/// `--state-dir PATH`. `None` is one on no named folder.
fn running(exe: &Path) -> Vec<Option<PathBuf>> {
    let listing = std::process::Command::new("/bin/ps")
        .args(["-axww", "-o", "command="])
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_default();
    copies(&listing, exe)
}

fn copies(listing: &str, exe: &Path) -> Vec<Option<PathBuf>> {
    let exe = exe.to_string_lossy();
    listing
        .lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix(exe.as_ref())?;
            match rest.strip_prefix(" --state-dir ") {
                _ if rest.is_empty() => Some(None),
                Some(dir) => Some(Some(PathBuf::from(dir))),
                None => None,
            }
        })
        .collect()
}

/// The folder an open copy shows when it is not `wanted`, which `open`
/// cannot change; `None` when none is open or one already shows `wanted`.
/// A copy on no named folder shows `default`.
fn open_elsewhere(running: &[Option<PathBuf>], wanted: &Path, default: &Path) -> Option<PathBuf> {
    let same = |a: &Path| {
        a == wanted || matches!((a.canonicalize(), wanted.canonicalize()), (Ok(a), Ok(b)) if a == b)
    };
    let folders: Vec<&Path> = running
        .iter()
        .map(|dir| dir.as_deref().unwrap_or(default))
        .collect();
    match folders.iter().any(|dir| same(dir)) {
        true => None,
        false => folders.first().map(|dir| dir.to_path_buf()),
    }
}

fn open_on(app: &Path, other: &Path, wanted: &Path) -> Failure {
    let mut failure = Failure::new(
        code::app_running,
        format!(
            "{} is already open on {}; it shows one state folder at a time, and opening it again only brings it to the front",
            app.display(),
            other.display()
        ),
    );
    failure.next = Some(
        format!(
            "quit Plow Latch, then run `latch app --state-dir {}` again",
            wanted.display()
        )
        .into(),
    );
    failure
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
    fn an_open_copy_is_found_with_its_folder() {
        let exe = Path::new("/Users/me/Applications/Plow Latch.app/Contents/MacOS/latch");
        let listing = "\
/sbin/launchd
/Users/me/Applications/Plow Latch.app/Contents/MacOS/latch
  /Users/me/Applications/Plow Latch.app/Contents/MacOS/latch --state-dir /Users/me/dev state
/Users/me/Applications/Plow Latch.app/Contents/MacOS/latch serve --foreground
/Users/me/.local/bin/latch --state-dir /Users/me/x
/Users/me/Applications/Plow Latch.app/Contents/MacOS/latchx --state-dir /y
";
        assert_eq!(
            copies(listing, exe),
            [None, Some(PathBuf::from("/Users/me/dev state"))]
        );
    }

    #[test]
    fn a_folder_other_than_the_open_copys_is_refused() {
        let default = Path::new("/Users/me/.latch");
        let (a, b) = (Path::new("/Users/me/a"), Path::new("/Users/me/b"));
        // None open, or one on the folder asked for: open it.
        assert_eq!(open_elsewhere(&[], b, default), None);
        assert_eq!(open_elsewhere(&[Some(b.into())], b, default), None);
        assert_eq!(open_elsewhere(&[None], default, default), None);
        // One on another folder, named or not: `open` would only bring it
        // forward there.
        assert_eq!(
            open_elsewhere(&[Some(a.into())], b, default),
            Some(a.into())
        );
        assert_eq!(open_elsewhere(&[None], b, default), Some(default.into()));
        let failure = open_on(Path::new("/Apps/Plow Latch.app"), a, b);
        assert_eq!(failure.code, "app_running");
        assert!(
            failure.message.contains("/Users/me/a"),
            "{}",
            failure.message
        );
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

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
//! folder and runs one copy, so while it is open (LaunchServices knows it
//! by its bundle id) a `latch app` naming a folder is refused unless the
//! app is on that very folder: the one it remembers in its defaults
//! (`StateDir`, PlowLatchApp.swift's `StateFolder`), else latch's default.

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
        let default = StateDir::new(crate::cli::BUILD_STATE_DIR.map(PathBuf::from))?.path;
        if let Some(why) = refused(&showing(&opened.app, &default), wanted) {
            return Err(open_on(&opened.app, &why, wanted));
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

/// Which state folder the app shows, if it is open.
#[derive(Debug, PartialEq)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
enum Showing {
    Closed,
    On(PathBuf),
    /// Open, on a folder latch could not read: why.
    Unknown(String),
}

#[cfg(target_os = "macos")]
fn showing(app: &Path, default: &Path) -> Showing {
    let Some(id) = bundle_id(app) else {
        return Showing::Unknown("its Info.plist names no bundle id".into());
    };
    if !is_open(&id) {
        return Showing::Closed;
    }
    // The app sets `StateDir` whenever it opens on a named folder and
    // removes it when it opens on the default, so while it is open the
    // key is the folder it runs on.
    match remembered(&id) {
        Ok(Some(dir)) => Showing::On(dir),
        Ok(None) => Showing::On(default.to_path_buf()),
        Err(why) => Showing::Unknown(why),
    }
}

/// The app's `StateDir` default, read as the app reads it.
#[cfg(target_os = "macos")]
fn remembered(id: &str) -> Result<Option<PathBuf>, String> {
    use core_foundation::base::{CFType, CFTypeRef, TCFType};
    use core_foundation::string::{CFString, CFStringRef};

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFPreferencesCopyAppValue(key: CFStringRef, app: CFStringRef) -> CFTypeRef;
    }
    let (key, id) = (CFString::from_static_string("StateDir"), CFString::new(id));
    // SAFETY: two live CFStrings; the value comes back retained, or null.
    let value =
        unsafe { CFPreferencesCopyAppValue(key.as_concrete_TypeRef(), id.as_concrete_TypeRef()) };
    if value.is_null() {
        return Ok(None);
    }
    // SAFETY: a retained CF object, released when this drops.
    let value = unsafe { CFType::wrap_under_create_rule(value) };
    match value.downcast::<CFString>() {
        Some(dir) => Ok(Some(PathBuf::from(dir.to_string()))),
        None => Err("its StateDir default is not a path".into()),
    }
}

#[cfg(not(target_os = "macos"))]
fn showing(_app: &Path, _default: &Path) -> Showing {
    Showing::Closed
}

#[cfg(target_os = "macos")]
fn bundle_id(app: &Path) -> Option<String> {
    use core_foundation::bundle::CFBundle;
    use core_foundation::url::CFURL;
    crate::gui::own_id(&CFBundle::new(CFURL::from_path(app, true)?)?)
}

/// Whether LaunchServices has an app with this bundle id running: only an
/// app it launched, never a latch command run from inside the bundle.
#[cfg(target_os = "macos")]
fn is_open(id: &str) -> bool {
    use core_foundation::base::TCFType;
    use core_foundation::string::CFString;
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};

    // Loaded here and not linked, as for the Camoufox handoff.
    let appkit = c"/System/Library/Frameworks/AppKit.framework/AppKit";
    // SAFETY: a NUL-terminated path; AppKit stays loaded once loaded.
    if unsafe { libc::dlopen(appkit.as_ptr(), libc::RTLD_LAZY) }.is_null() {
        return false;
    }
    let id = CFString::new(id);
    objc2::rc::autoreleasepool(|_| {
        let Some(class) = AnyClass::get(c"NSRunningApplication") else {
            return false;
        };
        // A CFString is an NSString (toll-free bridged).
        let id = id.as_concrete_TypeRef() as *const AnyObject;
        // SAFETY: a class method taking an NSString and returning an
        // autoreleased NSArray, never nil.
        let apps: *mut AnyObject =
            unsafe { msg_send![class, runningApplicationsWithBundleIdentifier: id] };
        if apps.is_null() {
            return false;
        }
        // SAFETY: `apps` is a live NSArray.
        let count: usize = unsafe { msg_send![apps, count] };
        count != 0
    })
}

/// Why the app can't be opened on `wanted` as it is, or `None` when it can.
fn refused(showing: &Showing, wanted: &Path) -> Option<String> {
    let same = |a: &Path| {
        a == wanted || matches!((a.canonicalize(), wanted.canonicalize()), (Ok(a), Ok(b)) if a == b)
    };
    match showing {
        Showing::Closed => None,
        Showing::On(dir) if same(dir) => None,
        Showing::On(dir) => Some(format!("is already open on {}", dir.display())),
        Showing::Unknown(why) => Some(format!(
            "is already open, and which state folder it shows is unknown ({why})"
        )),
    }
}

fn open_on(app: &Path, why: &str, wanted: &Path) -> Failure {
    let mut failure = Failure::new(
        code::app_running,
        format!(
            "{} {why}; it shows one state folder at a time, and opening it again only brings it to the front",
            app.display()
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

    /// The bundle around this latch, run directly or through a link as
    /// `~/.local/bin/latch` is.
    #[test]
    fn the_app_is_the_bundle_around_this_latch() {
        let dir = tempfile::tempdir().unwrap();
        let exe = bundle(dir.path());
        let link = dir.path().join("bin/latch");
        std::fs::create_dir(dir.path().join("bin")).unwrap();
        std::os::unix::fs::symlink(&exe, &link).unwrap();
        let app = std::fs::canonicalize(dir.path().join("Apps/Plow Latch.app")).unwrap();
        for path in [&exe, &link] {
            assert_eq!(bundle_of(path).as_ref(), Some(&app), "{}", path.display());
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
    fn a_folder_the_open_app_is_not_on_is_refused() {
        let default = Path::new("/Users/me/.latch");
        let (a, b) = (Path::new("/Users/me/a"), Path::new("/Users/me/b"));
        // Closed, or open on the folder asked for: open it.
        assert_eq!(refused(&Showing::Closed, b), None);
        assert_eq!(refused(&Showing::On(b.into()), b), None);
        assert_eq!(refused(&Showing::On(default.into()), default), None);
        // Opened with no arguments onto a folder it remembers: not the
        // default, though nothing named one.
        let why = refused(&Showing::On(a.into()), default).unwrap();
        assert!(why.contains("/Users/me/a"), "{why}");
        // On a folder latch could not read: never assumed to be the same.
        assert!(refused(&Showing::Unknown("no defaults".into()), b).is_some());
        let failure = open_on(Path::new("/Apps/Plow Latch.app"), &why, b);
        assert_eq!(failure.code, "app_running");
    }

    /// A host run from inside the bundle (`serve --foreground`, as
    /// launchd runs it), or `latch app` itself, is a command, not the app:
    /// the app is open only when LaunchServices launched it.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_command_run_from_the_bundle_is_not_the_open_app() {
        let dir = tempfile::tempdir().unwrap();
        let exe = bundle(dir.path());
        let app = exe.parent().unwrap().parent().unwrap().parent().unwrap();
        let id = format!("co.plow.latch.tests.{}", std::process::id());
        let plist = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\
             <key>CFBundleIdentifier</key><string>{id}</string>\
             <key>CFBundleExecutable</key><string>latch</string></dict></plist>\n"
        );
        std::fs::write(app.join("Contents/Info.plist"), plist).unwrap();
        std::fs::copy("/usr/bin/yes", &exe).unwrap();
        assert_eq!(bundle_id(app).as_deref(), Some(id.as_str()));
        let mut commands = [
            vec!["--state-dir", "/Users/me/b", "serve", "--foreground"],
            vec!["--state-dir", "/Users/me/b", "app"],
            vec![],
        ]
        .map(|args| {
            std::process::Command::new(&exe)
                .args(args)
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap()
        });
        std::thread::sleep(std::time::Duration::from_millis(200));
        let showing = showing(app, Path::new("/Users/me/.latch"));
        for command in &mut commands {
            command.kill().unwrap();
            command.wait().unwrap();
        }
        assert_eq!(showing, Showing::Closed);
    }

    /// The folder the app remembers is read from its own defaults, as the
    /// app writes them; none written is none remembered.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_folder_the_app_remembers_is_read_from_its_defaults() {
        let id = format!("co.plow.latch.tests.{}", std::process::id());
        let defaults = |args: &[&str]| {
            let status = std::process::Command::new("/usr/bin/defaults")
                .args(args)
                .status();
            assert!(status.unwrap().success(), "{args:?}");
        };
        assert_eq!(remembered(&id), Ok(None));
        defaults(&["write", &id, "StateDir", "/Users/me/dev state"]);
        let read = remembered(&id);
        defaults(&["delete", &id]);
        assert_eq!(read, Ok(Some(PathBuf::from("/Users/me/dev state"))));
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

//! The Mac app's window. Plow Latch.app's executable is latch itself; when
//! LaunchServices opens the app (Finder, the Dock, `open`, a login item),
//! latch loads the Swift interface from the bundle's Contents/Frameworks
//! (`latch/gui/`) and hands it the main thread. Every other run is the CLI,
//! which loads no AppKit or SwiftUI.
//!
//! A GUI launch is one LaunchServices made of this very bundle (its
//! `__CFBundleIdentifier` is the bundle's own id) with no arguments, or
//! exactly `--state-dir PATH` (`open APP --args --state-dir PATH`). The
//! window needs macOS 26 or later; below that the app says so and quits.

use std::ffi::{CString, OsString, c_char, c_int};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::process::ExitCode;

use core_foundation::base::TCFType;
use core_foundation::bundle::CFBundle;
use core_foundation::string::{CFString, CFStringRef};

use crate::failure::{Failure, code};
use crate::state::StateDir;

/// The library build.sh makes, in the bundle's Contents/Frameworks.
const LIBRARY: &str = "libPlowLatchUI.dylib";
/// The first macOS the window runs on.
const FIRST_MACOS: u32 = 26;

/// Runs the app if this is a GUI launch; `None` for every CLI run.
pub fn launch() -> Option<ExitCode> {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let state_dir = gui_args(&args)?;
    let launched = std::env::var("__CFBundleIdentifier").ok()?;
    let bundle = CFBundle::main_bundle();
    if own_id(&bundle).as_deref() != Some(launched.as_str()) {
        return None;
    }
    Some(match run(&bundle, state_dir) {
        Ok(exit) => exit,
        Err(failure) => {
            eprintln!("{}", failure.to_json());
            failure.exit.into()
        }
    })
}

/// A GUI launch's arguments: none, or exactly `--state-dir PATH`.
fn gui_args(args: &[OsString]) -> Option<Option<PathBuf>> {
    match args {
        [] => Some(None),
        [flag, path] if flag == "--state-dir" => Some(Some(PathBuf::from(path))),
        _ => None,
    }
}

fn own_id(bundle: &CFBundle) -> Option<String> {
    let key = CFString::from_static_string("CFBundleIdentifier");
    let id = bundle
        .info_dictionary()
        .find(&key)?
        .downcast::<CFString>()?;
    Some(id.to_string())
}

fn run(bundle: &CFBundle, state_dir: Option<PathBuf>) -> Result<ExitCode, Failure> {
    if !runs_window(&macos_version()) {
        too_old();
        return Ok(ExitCode::SUCCESS);
    }
    let default = StateDir::new(crate::cli::BUILD_STATE_DIR.map(PathBuf::from))?;
    let launched = launched_on(state_dir.as_deref(), &default.path);
    let state = StateDir::new(state_dir.or(Some(default.path)))?;
    let latch = std::env::current_exe().map_err(|err| missing(err.to_string()))?;
    let library = bundle
        .private_frameworks_url()
        .and_then(|url| url.to_path())
        .ok_or_else(|| missing("the app has no Frameworks folder".into()))?
        .join(LIBRARY);
    let c = |path: &std::path::Path| CString::new(path.as_os_str().as_bytes()).unwrap();
    let (library, latch, state) = (c(&library), c(&latch), c(&state.path));
    let launched = launched as c_int;
    // SAFETY: a NUL-terminated path; the library stays loaded for the life
    // of the process.
    let handle = unsafe { libc::dlopen(library.as_ptr(), libc::RTLD_NOW) };
    if handle.is_null() {
        // SAFETY: dlerror returns the last dlopen error as a C string.
        let why = unsafe { std::ffi::CStr::from_ptr(libc::dlerror()) };
        return Err(missing(why.to_string_lossy().into_owned()));
    }
    // SAFETY: a live handle and a NUL-terminated symbol name.
    let entry = unsafe { libc::dlsym(handle, c"latch_gui_main".as_ptr()) };
    if entry.is_null() {
        return Err(missing("its library has no latch_gui_main".into()));
    }
    // SAFETY: latch_gui_main is `(const char *latch, const char *state_dir,
    // int launched_on) -> int` (a `Launched`)
    // (latch/gui/Sources/PlowLatchUI/App/PlowLatchApp.swift); it runs the
    // app on this, the main, thread and copies both strings first.
    let main: extern "C" fn(*const c_char, *const c_char, c_int) -> c_int =
        unsafe { std::mem::transmute(entry) };
    Ok(match main(latch.as_ptr(), state.as_ptr(), launched) {
        0 => ExitCode::SUCCESS,
        _ => ExitCode::FAILURE,
    })
}

/// Which state folder a launch names, as the window is told: the window
/// remembers a `--state-dir` of its own and comes back to it on a launch
/// with no arguments, such as macOS's Quit & Reopen after Full Disk
/// Access is granted, which drops them.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Launched {
    /// A `--state-dir` other than latch's default.
    Other = 0,
    /// No `--state-dir`.
    Unnamed = 1,
    /// A `--state-dir` naming latch's default.
    Default = 2,
}

fn launched_on(state_dir: Option<&std::path::Path>, default: &std::path::Path) -> Launched {
    let same = |a: &std::path::Path, b: &std::path::Path| {
        a == b || matches!((a.canonicalize(), b.canonicalize()), (Ok(a), Ok(b)) if a == b)
    };
    match state_dir {
        None => Launched::Unnamed,
        Some(path) if same(path, default) => Launched::Default,
        Some(_) => Launched::Other,
    }
}

fn missing(why: String) -> Failure {
    let mut failure = Failure::new(
        code::app_broken,
        format!("the app's window could not load ({why})"),
    );
    failure.next = Some("reinstall Latch".into());
    failure
}

/// This Mac's macOS version, e.g. "26.1", or "" if macOS won't say.
fn macos_version() -> String {
    let mut buf = [0u8; 32];
    let mut len = buf.len();
    // SAFETY: a NUL-terminated name and a buffer of `len` bytes.
    let ok = unsafe {
        libc::sysctlbyname(
            c"kern.osproductversion".as_ptr(),
            buf.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    } == 0;
    let text = if ok { &buf[..len] } else { &[][..] };
    String::from_utf8_lossy(text)
        .trim_end_matches('\0')
        .to_string()
}

/// Whether the window runs on this macOS version.
fn runs_window(version: &str) -> bool {
    let major = version
        .split('.')
        .next()
        .and_then(|major| major.parse::<u32>().ok());
    major.is_some_and(|major| major >= FIRST_MACOS)
}

/// Below macOS 26: one alert, with no AppKit, and the app quits.
fn too_old() {
    unsafe extern "C" {
        fn CFUserNotificationDisplayAlert(
            timeout: f64,
            flags: usize,
            icon: *const std::ffi::c_void,
            sound: *const std::ffi::c_void,
            localization: *const std::ffi::c_void,
            header: CFStringRef,
            message: CFStringRef,
            default_button: CFStringRef,
            alternate_button: CFStringRef,
            other_button: CFStringRef,
            response: *mut usize,
        ) -> i32;
    }
    let header = CFString::new("Latch's window needs macOS 26 or later.");
    let message =
        CFString::new("On this Mac, set up Latch in Terminal instead: run `latch setup`.");
    let ok = CFString::new("OK");
    let mut response = 0;
    // SAFETY: live CFStrings for the texts; null for the optional icon,
    // sound, localization and other buttons; flags 0 is a plain alert.
    unsafe {
        CFUserNotificationDisplayAlert(
            0.0,
            0,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            header.as_concrete_TypeRef(),
            message.as_concrete_TypeRef(),
            ok.as_concrete_TypeRef(),
            std::ptr::null(),
            std::ptr::null(),
            &mut response,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_gui_launch_has_no_arguments_or_exactly_a_state_dir() {
        let args = |list: &[&str]| list.iter().map(OsString::from).collect::<Vec<_>>();
        assert_eq!(gui_args(&args(&[])), Some(None));
        assert_eq!(
            gui_args(&args(&["--state-dir", "/tmp/x"])),
            Some(Some(PathBuf::from("/tmp/x")))
        );
        for cli in [
            &["status", "--local"][..],
            &["--state-dir"],
            &["--state-dir", "/tmp/x", "status"],
            &["status", "--state-dir", "/tmp/x"],
            &["--version"],
        ] {
            assert_eq!(gui_args(&args(cli)), None, "{cli:?}");
        }
    }

    #[test]
    fn a_launch_says_which_state_folder_it_named() {
        let default = std::path::Path::new("/Users/me/.latch");
        assert_eq!(launched_on(None, default), Launched::Unnamed);
        assert_eq!(launched_on(Some(default), default), Launched::Default);
        assert_eq!(
            launched_on(Some(std::path::Path::new("/tmp/dev")), default),
            Launched::Other
        );
    }

    #[test]
    fn the_window_needs_macos_26() {
        for version in ["26.0", "26.1.2", "27.0.1", "30"] {
            assert!(runs_window(version), "{version}");
        }
        for version in ["15.7", "13.0", "", "x"] {
            assert!(!runs_window(version), "{version}");
        }
    }

    #[test]
    fn this_macs_version_is_read() {
        assert!(!macos_version().is_empty());
    }
}

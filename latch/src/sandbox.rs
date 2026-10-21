//! The worker sandbox: Seatbelt on macOS, bubblewrap and seccomp on Linux.
//!
//! A worker process calls `confine` before starting any runtime or threads.
//! From then on the operating system itself enforces the policy, and it
//! cannot be lifted, not even by programs the worker starts: writes only
//! inside the host's areas (and an `exec` call's own temp folder), never the
//! host's state directory or the running latch. File and `exec` workers read
//! the whole filesystem, as Codex CLI's do; Google workers read only their
//! call's folder and the system locations programs need. A file worker gets no
//! network; `exec` and Google workers may reach out to the network. Google
//! can execute only its pinned binary; neither can send Apple Events. The
//! kernel resolves every path (symlinks included) before checking it, so a
//! link into a hidden path is refused by Seatbelt and reports `not_found`
//! in Linux's filesystem view.
//!
//! Linux hides the state directory with an empty, read-only mount, even when
//! an area contains it. Other systems refuse to run a worker at all.

use std::path::PathBuf;

/// What a worker may touch.
#[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
pub struct Policy {
    /// Directories the worker may read and write, with everything below them.
    pub areas: Vec<PathBuf>,
    /// The host's state directory: denied on macOS, hidden on Linux, even in an area.
    pub state_dir: PathBuf,
    /// An `exec` worker: may start programs and use the outbound network.
    pub exec: bool,
    /// An `exec` worker's own temp folder, made by the host for this call:
    /// readable and writable on macOS. On Linux the worker's private `/tmp`
    /// takes its place, and the folder only holds bubblewrap's status file.
    pub tmpdir: Option<PathBuf>,
    /// A Google worker may run only this pinned program.
    pub program: Option<PathBuf>,
    /// Test only: make the OS refuse the sandbox, to show that a sandbox
    /// that cannot be applied fails the operation.
    pub break_for_test: bool,
}

/// Confines the calling thread and any threads or processes it creates.
/// Call before starting other threads: the Linux launcher replaces this process.
/// An error means the process is not confined and must not go on.
pub fn confine(policy: &Policy) -> Result<(), String> {
    platform::confine(policy)
}

/// What a Linux browser worker sees: the system files its engine and
/// display need, read-only, and of the state directory only its profile
/// (read-write) and its engine's plugin folder (read-only); its scratch
/// folder; a private `/tmp`; and the outbound network. No areas.
#[cfg(target_os = "linux")]
#[allow(dead_code)] // the browser worker confines itself with it
pub struct BrowserView {
    pub state_dir: PathBuf,
    pub profile: PathBuf,
    pub plugin: PathBuf,
    pub scratch: PathBuf,
}

/// Confines a browser worker as `confine` does other workers, to `view`.
/// The engine's own sandbox runs inside it, in namespaces of its own.
#[cfg(target_os = "linux")]
#[allow(dead_code)] // the browser worker confines itself with it
pub fn confine_browser(view: &BrowserView) -> Result<(), String> {
    platform::confine_browser(view)
}

/// Checks, without confining anything, that this system can apply the
/// worker sandbox at all. The host runs it once at startup, so a kernel
/// without the sandbox is refused there, not by every call later.
pub fn check() -> Result<(), String> {
    platform::check()
}

pub fn name() -> &'static str {
    if cfg!(target_os = "linux") {
        "bubblewrap + seccomp"
    } else {
        "Seatbelt"
    }
}

/// Preserve a launcher's diagnostic when it fails before the worker starts.
pub fn startup_error(stderr: &str) -> String {
    #[cfg(target_os = "linux")]
    {
        let cause = stderr.trim();
        let mut message = format!(
            "bubblewrap worker setup failed: {cause}. Run install.sh; check that user namespaces and mounts are permitted by the host policy."
        );
        if std::path::Path::new("/.dockerenv").exists()
            || std::path::Path::new("/run/.containerenv").exists()
        {
            message.push_str("\nDocker: use non-root --user, --cap-drop=ALL, --security-opt=no-new-privileges=true, --security-opt=seccomp=/path/to/latch/docker/seccomp.json.\nAppArmor hosts also need a loaded, validated container profile.\nDo not use --privileged or SYS_ADMIN. See 'Running a latch host in Docker' (latch/DOCKER.md).");
        }
        message
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = stderr;
        "the probe stopped without saying why".to_string()
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::{CStr, CString, c_char, c_int};
    use std::path::{Path, PathBuf};

    use super::Policy;

    // `sandbox_init` is part of macOS's system library. Apple marks it
    // deprecated but it is what `sandbox-exec` and other tools still use.
    unsafe extern "C" {
        fn sandbox_init(profile: *const c_char, flags: u64, errorbuf: *mut *mut c_char) -> c_int;
        fn sandbox_free_error(errorbuf: *mut c_char);
    }

    /// Seatbelt is part of every macOS, so there is nothing to probe.
    pub fn check() -> Result<(), String> {
        Ok(())
    }

    pub fn confine(policy: &Policy) -> Result<(), String> {
        let profile = profile(policy)?;
        if policy.break_for_test {
            // macOS never applies a second, different profile to a process
            // that already has one, so a valid one first makes the real one
            // fail every time. (An invalid profile is no reliable failure:
            // libsandbox sometimes applies what it could parse of it.)
            apply("(version 1) (allow default)")?;
        }
        apply(&profile)
    }

    fn apply(profile: &str) -> Result<(), String> {
        let profile =
            CString::new(profile).map_err(|_| "profile contains a NUL byte".to_string())?;
        let mut error: *mut c_char = std::ptr::null_mut();
        // SAFETY: `profile` is a valid C string for the duration of the call,
        // and `error` is either left null or set to a message we free below.
        let status = unsafe { sandbox_init(profile.as_ptr(), 0, &mut error) };
        if status == 0 {
            return Ok(());
        }
        let message = if error.is_null() {
            std::io::Error::last_os_error().to_string()
        } else {
            // SAFETY: on failure `error` points to a C string owned by the
            // sandbox library, which we copy and then hand back to free.
            let text = unsafe { CStr::from_ptr(error) }
                .to_string_lossy()
                .into_owned();
            unsafe { sandbox_free_error(error) };
            text
        };
        Err(format!("the sandbox could not be applied: {message}"))
    }

    /// The Seatbelt profile (a small Lisp-like language). Later rules win
    /// over earlier ones, so the protections common to every kind come last.
    fn profile(policy: &Policy) -> Result<String, String> {
        let mut rules = if policy.exec {
            exec_rules(policy)?
        } else {
            narrow_rules(policy)?
        };
        rules.extend(protections(policy)?);
        Ok(rules.join("\n"))
    }

    /// File and Google workers: deny by default, the system files programs
    /// need to start, and the areas (file workers also read everything).
    fn narrow_rules(policy: &Policy) -> Result<Vec<String>, String> {
        let mut rules = vec![
            "(version 1)".to_string(),
            "(deny default)".to_string(),
            // What any program needs to keep running: the system's own files.
            "(allow file-read* (literal \"/\") (subpath \"/usr\") (subpath \"/System\") (subpath \"/bin\") \
             (subpath \"/private/var/db/dyld\") (literal \"/dev/null\") (literal \"/dev/urandom\"))"
                .to_string(),
            "(allow sysctl-read)".to_string(),
        ];
        // A file worker reads the whole filesystem, as `exec` does; it
        // still writes only in the areas, and the state folder stays denied.
        if policy.program.is_none() {
            rules.push("(allow file-read*)".to_string());
        }
        if let Some(program) = &policy.program {
            rules.push(format!(
                "(allow file-read* process-exec (literal {}))",
                quoted(program)?
            ));
            rules.push("(allow process-fork)".to_string());
            // Apple's shared network baseline includes the system resolver
            // and notification shared memory used by getaddrinfo.
            rules.push("(import \"system.sb\") (system-network) (allow ipc-posix-shm (ipc-posix-name \"apple.shm.notification_center\"))".to_string());
            rules.push("(allow file-write-data (literal \"/dev/null\"))".to_string());
            rules.push("(allow network-outbound)".to_string());
            rules.push("(deny appleevent-send)".to_string());
            rules.push("(allow file-read-metadata)".to_string());
            rules.push("(allow file-read* (subpath \"/private/etc\") (literal \"/private/var/run/resolv.conf\") (subpath \"/Library/Keychains\"))".to_string());
            rules.push("(allow mach-lookup (global-name \"com.apple.SystemConfiguration.configd\") (global-name \"com.apple.SystemConfiguration.DNSConfiguration\") (global-name \"com.apple.trustd.agent\") (global-name \"com.apple.trustd\"))".to_string());
        }
        for area in &policy.areas {
            // Looking up a path inside an area (as `pwd` does) needs the
            // details, not the contents, of each directory above it.
            let real = area
                .canonicalize()
                .map_err(|err| format!("{}: {err}", area.display()))?;
            let mut parent = real.parent();
            while let Some(dir) = parent {
                rules.push(format!(
                    "(allow file-read-metadata (literal {}))",
                    quoted(dir)?
                ));
                parent = dir.parent();
            }
            rules.push(format!(
                "(allow file-read* file-write* (subpath {}))",
                quoted(area)?
            ));
        }
        Ok(rules)
    }

    /// `exec` workers follow Codex CLI's macOS sandbox (vendored, see
    /// NOTICE): its base and network rules, reads of the whole filesystem
    /// (what the user can read; macOS privacy protections still apply) and
    /// preferences, and writes only in the areas and the call's temp folder.
    /// Outbound network only, no Apple Events.
    fn exec_rules(policy: &Policy) -> Result<Vec<String>, String> {
        let mut rules = vec![CODEX_BASE.to_string(), "(allow file-read*)".to_string()];
        for root in policy.areas.iter().chain(&policy.tmpdir) {
            rules.push(format!(
                "(allow file-read* file-write* (subpath {}))",
                quoted(root)?
            ));
        }
        rules.extend([
            "(allow network-outbound)".to_string(),
            "(allow system-socket (socket-domain AF_UNIX))".to_string(),
            CODEX_NETWORK.to_string(),
            CODEX_PREFERENCES.to_string(),
            // As Codex: no embedded XPC services, except the speech
            // synthesizer `say` uses, and no fcntls that change a file
            // through a read-only descriptor (F_MAKECOMPRESSED,
            // F_TRANSFEREXTENTS).
            SPEECH_XPC_EXCEPTION.to_string(),
            "(deny system-fcntl (fcntl-command 80 110))".to_string(),
            SPEECH_AND_AUDIO.to_string(),
            "(deny appleevent-send)".to_string(),
        ]);
        Ok(rules)
    }

    /// Every embedded XPC service is denied but the speech synthesizer's.
    const SPEECH_XPC_EXCEPTION: &str = "(deny mach-lookup
  (require-all
    (xpc-service-name-prefix \"\")
    (require-not
      (xpc-service-name
        \"com.apple.ax.MauiTTSSupport.MauiAUSP\"
        \"com.apple.ax.MauiTTSSupport.MauiAUSP.apple-extension-service\"))))";

    /// What `say` needs, measured on macOS 27 with the default voice: the
    /// audio component registry, plugin lookup, the process manager and the
    /// synthesizer's service find and load the voice; `audiohald` plays it.
    /// Named services only, no blanket `mach-lookup`. Speech still needs a
    /// host started in the logged-in (Aqua) session: from a background
    /// session macOS synthesizes silence under any sandbox.
    const SPEECH_AND_AUDIO: &str = "(allow mach-lookup
  (global-name \"com.apple.audio.AudioComponentRegistrar\")
  (global-name \"com.apple.pluginkit.pkd\")
  (global-name \"com.apple.runningboard\")
  (global-name \"com.apple.audio.audiohald\")
  (xpc-service-name \"com.apple.ax.MauiTTSSupport.MauiAUSP\")
  (xpc-service-name \"com.apple.ax.MauiTTSSupport.MauiAUSP.apple-extension-service\"))";

    const CODEX_BASE: &str = include_str!("sandbox/codex/seatbelt_base_policy.sbpl");
    const CODEX_NETWORK: &str = include_str!("sandbox/codex/seatbelt_network_policy.sbpl");
    const CODEX_PREFERENCES: &str = include_str!("sandbox/codex/seatbelt_preferences_policy.sbpl");

    /// For every kind, last: the state directory can be neither read nor
    /// written, and the running latch never written. Seatbelt matches
    /// paths, so renaming a folder above either would move it out of reach
    /// of these rules; directories above them inside a writable folder, and
    /// the writable folders themselves, cannot be renamed or removed.
    fn protections(policy: &Policy) -> Result<Vec<String>, String> {
        let real = |path: &Path| {
            path.canonicalize()
                .map_err(|err| format!("{}: {err}", path.display()))
        };
        let state = real(&policy.state_dir)?;
        let latch = real(&std::env::current_exe().map_err(|err| err.to_string())?)?;
        let mut writable = Vec::new();
        for root in policy.areas.iter().chain(&policy.tmpdir) {
            writable.push(real(root)?);
        }
        let mut rules = vec![
            // Not even run: a plugin there is its own worker's alone.
            format!(
                "(deny file-read* file-write* process-exec (subpath {}))",
                quoted(&state)?
            ),
            format!("(deny file-write* (literal {}))", quoted(&latch)?),
        ];
        // The one part of the state folder a worker may see: a Google
        // worker reads, and runs, its own plugin's folder (gog's version
        // folder), never writes it.
        if let Some(program) = &policy.program {
            let program = real(program)?;
            let folder = program.parent().unwrap_or(&program);
            rules.push(format!("(allow file-read* (subpath {}))", quoted(folder)?));
            rules.push(format!(
                "(allow process-exec (literal {}))",
                quoted(&program)?
            ));
        }
        let mut anchors: Vec<&Path> = writable.iter().map(PathBuf::as_path).collect();
        for protected in [&state, &latch] {
            for above in protected.ancestors().skip(1) {
                if writable.iter().any(|root| above.starts_with(root)) {
                    anchors.push(above);
                }
            }
        }
        anchors.sort();
        anchors.dedup();
        for dir in anchors {
            rules.push(format!(
                "(deny file-write-unlink (require-all (vnode-type DIRECTORY) (literal {})))",
                quoted(dir)?
            ));
        }
        Ok(rules)
    }

    /// A directory as a profile string. Seatbelt compares real paths (on
    /// macOS `/tmp` is really `/private/tmp`), so the path is resolved first.
    fn quoted(path: &Path) -> Result<String, String> {
        let real = path
            .canonicalize()
            .map_err(|err| format!("{}: {err}", path.display()))?;
        let text = real
            .to_str()
            .ok_or_else(|| format!("{} is not valid UTF-8", real.display()))?;
        // The profile language quotes like JSON: escape `\` and `"`.
        Ok(format!(
            "\"{}\"",
            text.replace('\\', "\\\\").replace('"', "\\\"")
        ))
    }
}

#[cfg(target_os = "linux")]
#[path = "sandbox/linux.rs"]
mod platform;

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod platform {
    use super::Policy;

    pub fn check() -> Result<(), String> {
        Err("no worker sandbox on this operating system yet".to_string())
    }

    pub fn confine(_policy: &Policy) -> Result<(), String> {
        Err("no worker sandbox on this operating system yet".to_string())
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod google_tests {
    use super::*;
    use std::{fs, process::Command};
    #[test]
    fn each_google_worker_can_read_only_its_private_call() {
        if let Ok(root) = std::env::var("LATCH_PRIVATE_CALL_TEST") {
            let root = PathBuf::from(root);
            let call = root.join(std::env::var("LATCH_PRIVATE_CALL_NAME").unwrap());
            let inside_state = call.starts_with(root.join("state"));
            let confined = confine(&Policy {
                areas: vec![call.clone()],
                state_dir: root.join("state"),
                exec: false,
                tmpdir: None,
                program: Some(PathBuf::from(
                    std::env::var_os("LATCH_PRIVATE_PROGRAM").unwrap(),
                )),
                break_for_test: false,
            });
            if inside_state {
                if confined.is_ok() {
                    assert!(
                        fs::read(call.join("input")).is_err(),
                        "state must stay denied even if named as the Google area"
                    );
                }
                return;
            }
            confined.unwrap();
            assert_eq!(fs::read(call.join("input")).unwrap(), b"own input");
            for path in [
                "state/secret",
                "state/call/input",
                "private/other-call/input",
                "area/inside",
            ] {
                assert!(fs::read(root.join(path)).is_err(), "{path} must be private");
            }
            return;
        }
        let parent = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.dev/tests");
        fs::create_dir_all(&parent).unwrap();
        let dir = tempfile::tempdir_in(parent).unwrap();
        for name in ["area", "state/call", "private/call", "private/other-call"] {
            fs::create_dir_all(dir.path().join(name)).unwrap();
        }
        for (name, data) in [
            ("area/inside", "configured area"),
            ("state/secret", "secret"),
            ("private/call/input", "own input"),
            ("state/call/input", "state input"),
            ("private/other-call/input", "other input"),
        ] {
            fs::write(dir.path().join(name), data).unwrap();
        }
        for call in ["private/call", "state/call"] {
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "sandbox::google_tests::each_google_worker_can_read_only_its_private_call",
                    "--nocapture",
                ])
                .env(
                    "LATCH_PRIVATE_CALL_TEST",
                    dir.path().canonicalize().unwrap(),
                )
                .current_dir(dir.path().join(call))
                .env("LATCH_PRIVATE_CALL_NAME", call)
                .env("LATCH_PRIVATE_PROGRAM", std::env::current_exe().unwrap())
                .output()
                .unwrap();
            if cfg!(target_os = "linux") && call == "state/call" {
                // Hiding state also hides this deliberately invalid working directory.
                assert!(!output.status.success(), "{output:?}");
            } else {
                assert!(output.status.success(), "{call}: {output:?}");
            }
        }
    }

    #[test]
    fn only_a_google_worker_sees_its_own_plugin_folder() {
        if let Ok(root) = std::env::var("LATCH_PLUGIN_TEST") {
            let root = PathBuf::from(root);
            let mode = std::env::var("LATCH_PLUGIN_MODE").unwrap();
            let gog = root.join("state/plugins/gog/0.1/gog");
            confine(&Policy {
                areas: vec![root.join("private/call")],
                state_dir: root.join("state"),
                exec: mode == "exec",
                tmpdir: (mode == "exec").then(|| root.join("private/call")),
                program: (mode == "google").then(|| gog.clone()),
                break_for_test: false,
            })
            .unwrap();
            assert_eq!(
                fs::read(&gog).is_ok(),
                mode == "google",
                "{mode} reading gog"
            );
            // Only the Google worker may run it: exec and file workers may
            // not even start a program from the state folder.
            let ran = Command::new(&gog).arg("--list").output();
            assert_eq!(
                ran.is_ok_and(|out| out.status.success()),
                mode == "google",
                "{mode} running gog"
            );
            for other in ["state/secret", "state/plugins/other/1/tool"] {
                assert!(
                    fs::read(root.join(other)).is_err(),
                    "{mode} reading {other}"
                );
            }
            // Read-only, even for its own worker.
            assert!(fs::write(root.join("state/plugins/gog/0.1/new"), b"x").is_err());
            return;
        }
        let parent = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.dev/tests");
        fs::create_dir_all(&parent).unwrap();
        let dir = tempfile::tempdir_in(parent).unwrap();
        for name in [
            "private/call",
            "state/plugins/gog/0.1",
            "state/plugins/other/1",
        ] {
            fs::create_dir_all(dir.path().join(name)).unwrap();
        }
        fs::write(dir.path().join("state/secret"), "secret").unwrap();
        fs::write(dir.path().join("state/plugins/other/1/tool"), "other").unwrap();
        fs::copy(
            std::env::current_exe().unwrap(),
            dir.path().join("state/plugins/gog/0.1/gog"),
        )
        .unwrap();
        for mode in ["google", "exec", "file"] {
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "sandbox::google_tests::only_a_google_worker_sees_its_own_plugin_folder",
                    "--nocapture",
                ])
                .env("LATCH_PLUGIN_TEST", dir.path().canonicalize().unwrap())
                .env("LATCH_PLUGIN_MODE", mode)
                .current_dir(dir.path().join("private/call"))
                .output()
                .unwrap();
            assert!(output.status.success(), "{mode}: {output:?}");
        }
    }
}

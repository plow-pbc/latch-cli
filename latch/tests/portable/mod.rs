//! What the tests need of the operating system, asked the same way on every
//! one: a POSIX shell to run a script with, a path as a script names it, a
//! program made of a script, whether a file is its owner's alone, and ending,
//! pausing and resuming a process. Unit tests include it as
//! `crate::portable`, integration tests as `mod portable`.
//!
//! On Windows the shell is busybox-w32's `sh` (`LATCH_TEST_SH`, or
//! `.dev/tools/sh.exe`, which `scripts/test.py` fetches at a pinned hash):
//! it runs inside latch's AppContainer, as MSYS's `sh` (Git's) does not.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

/// The shell a test runs a script with: `[sh(), "-c", script]`.
pub fn sh() -> PathBuf {
    #[cfg(unix)]
    return PathBuf::from("/bin/sh");
    #[cfg(windows)]
    {
        static SH: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        SH.get_or_init(|| {
            let sh = std::env::var_os("LATCH_TEST_SH").map_or_else(
                || Path::new(env!("CARGO_MANIFEST_DIR")).join("../.dev/tools/sh.exe"),
                PathBuf::from,
            );
            assert!(
                sh.is_file(),
                "no shell for the tests at {}: run them with `just test`, which puts \
                 busybox-w32's sh.exe there, or set LATCH_TEST_SH",
                sh.display()
            );
            // Programs inside latch's sandbox run it too: every container may
            // read and run what is in its folder, as in Program Files.
            let folder = sh.parent().unwrap();
            if !windows::allowed(folder)
                .iter()
                .any(|sid| sid == "S-1-15-2-1")
            {
                let granted = std::process::Command::new("icacls")
                    .arg(folder)
                    .args(["/grant", "*S-1-15-2-1:(OI)(CI)(RX)", "/Q"])
                    .output()
                    .unwrap();
                assert!(granted.status.success(), "{granted:?}");
            }
            sh
        })
        .clone()
    }
}

/// A program a test runs by name (`true`, `cat`): on Unix that name, which
/// PATH finds; on Windows busybox-w32 named for it beside the tests' shell,
/// which it then runs as.
pub fn tool(name: &str) -> String {
    #[cfg(unix)]
    return name.to_string();
    #[cfg(windows)]
    {
        let sh = sh();
        let tool = sh.with_file_name(format!("{name}.exe"));
        if !tool.exists() {
            // Whole or not at all: tests run side by side.
            let staged = tool.with_extension(format!(
                "{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::copy(&sh, &staged).unwrap();
            if std::fs::rename(&staged, &tool).is_err() {
                std::fs::remove_file(&staged).unwrap();
            }
        }
        tool.display().to_string()
    }
}

/// Has `command`'s program find the tests' programs by name, as it does on
/// Unix: on Windows, by busybox-w32 named for each, first on its PATH.
pub fn find_tools(command: &mut std::process::Command) -> &mut std::process::Command {
    #[cfg(windows)]
    {
        for name in ["cat", "echo", "false", "touch", "true"] {
            tool(name);
        }
        let path = std::env::var_os("PATH").unwrap_or_default();
        let folders = std::iter::once(sh().parent().unwrap().to_path_buf())
            .chain(std::env::split_paths(&path));
        command.env("PATH", std::env::join_paths(folders).unwrap());
    }
    command
}

/// The PATH of a host started with no shell (by the Mac app, a login item,
/// a scheduled task), the tests' programs still on it.
pub fn bare_path() -> std::ffi::OsString {
    if cfg!(windows) {
        tool("true");
        sh().parent().unwrap().as_os_str().to_os_string()
    } else {
        "/usr/bin:/bin".into()
    }
}

/// Makes `home` what `command`'s program takes for its home: `HOME`, and on
/// Windows `USERPROFILE`, which it reads there.
pub fn set_home(
    command: &mut std::process::Command,
    home: impl AsRef<std::ffi::OsStr>,
) -> &mut std::process::Command {
    if cfg!(windows) {
        command.env("USERPROFILE", home.as_ref());
    }
    command.env("HOME", home)
}

/// `path` by its real path, as a person writes it: without the `\\?\` of
/// Windows' canonical form, which cmd.exe cannot run a program from.
pub fn real_path(path: &Path) -> PathBuf {
    let real = path.canonicalize().unwrap();
    match real.to_str().and_then(|text| text.strip_prefix(r"\\?\")) {
        Some(plain) if cfg!(windows) => PathBuf::from(plain),
        _ => real,
    }
}

/// Where a host says who holds its `serve.lock`: that file, or on Windows
/// beside it, where a locked file can't be read.
pub fn lock_holder(state: &Path) -> PathBuf {
    state.join(if cfg!(windows) {
        "serve.holder"
    } else {
        "serve.lock"
    })
}

/// A secret file of the state folder `state`, opened: as it is on disk, or
/// on Windows sealed for this user and that file (latch's secrets/dpapi.rs).
pub fn secret_file(state: &Path, file: &str) -> Vec<u8> {
    let bytes = std::fs::read(state.join(file)).unwrap();
    #[cfg(not(windows))]
    return bytes;
    #[cfg(windows)]
    {
        use std::ptr::{null, null_mut};
        use windows_sys::Win32::Foundation::LocalFree;
        use windows_sys::Win32::Security::Cryptography::{
            CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptUnprotectData,
        };
        let sealed = bytes.strip_prefix(b"latch-dpapi-1\n".as_slice()).unwrap();
        let account = format!("{}/{file}", state.canonicalize().unwrap().display());
        let blob = |bytes: &[u8]| CRYPT_INTEGER_BLOB {
            cbData: bytes.len() as u32,
            pbData: bytes.as_ptr().cast_mut(),
        };
        let mut opened = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: null_mut(),
        };
        // SAFETY: both blobs point at live slices for the call; DPAPI
        // allocates `opened`, freed below once copied.
        let ok = unsafe {
            CryptUnprotectData(
                &blob(sealed),
                null_mut(),
                &blob(account.as_bytes()),
                null(),
                null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut opened,
            )
        };
        assert_ne!(ok, 0, "{}", std::io::Error::last_os_error());
        // SAFETY: what DPAPI allocated, read once and freed once.
        unsafe {
            let plain = std::slice::from_raw_parts(opened.pbData, opened.cbData as usize).to_vec();
            LocalFree(opened.pbData.cast());
            plain
        }
    }
}

/// An argv that runs `script` in the tests' shell.
pub fn script(script: &str) -> Vec<String> {
    vec![sh().display().to_string(), "-c".into(), script.into()]
}

/// `path` as a script names it: Windows' backslashes, which a shell reads
/// as escapes, become slashes.
pub fn script_path(path: &Path) -> String {
    let text = path.display().to_string();
    if cfg!(windows) {
        let plain = text.strip_prefix(r"\\?\").unwrap_or(&text);
        plain.replace('\\', "/")
    } else {
        text
    }
}

/// A program at `path` that runs `body` in the tests' shell, its arguments
/// as `"$@"`: on Unix the script itself, made executable; on Windows a
/// `.cmd` beside it, which runs it (the path answered).
pub fn program(path: &Path, body: &str) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, format!("#!/bin/sh\n{body}")).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.to_path_buf()
    }
    #[cfg(windows)]
    {
        let script = path.with_extension("sh");
        std::fs::write(&script, body).unwrap();
        let program = path.with_extension("cmd");
        std::fs::write(
            &program,
            format!("@\"{}\" \"{}\" %*\r\n", sh().display(), script.display()),
        )
        .unwrap();
        program
    }
}

/// A link at `link` to the folder `target`: a symlink, or on Windows a
/// junction, which needs no privilege (a symlink there does).
pub fn link_folder(target: &Path, link: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).unwrap();
    #[cfg(windows)]
    {
        let plain = |path: &Path| {
            let text = path.display().to_string();
            text.strip_prefix(r"\\?\").unwrap_or(&text).to_string()
        };
        let made = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J", &plain(link), &plain(target)])
            .output()
            .unwrap();
        assert!(made.status.success(), "{made:?}");
    }
}

/// Removes the link `link_folder` made, not what it leads to.
pub fn unlink_folder(link: &Path) {
    #[cfg(unix)]
    std::fs::remove_file(link).unwrap();
    #[cfg(windows)]
    std::fs::remove_dir(link).unwrap();
}

/// A file outside home that every user may read: `/etc/hosts`, or
/// Windows' own hosts file.
pub fn system_file() -> PathBuf {
    if cfg!(windows) {
        let root = std::env::var_os("SystemRoot").unwrap();
        Path::new(&root).join(r"System32\drivers\etc\hosts")
    } else {
        PathBuf::from("/etc/hosts")
    }
}

/// Whether `path` lets in its owner alone: no group or other mode bits on
/// Unix; on Windows, an access list that allows this user and no one else
/// but the machine's own (SYSTEM and Administrators, root's part there).
pub fn private(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o077 == 0
    }
    #[cfg(windows)]
    {
        let user = windows::user();
        windows::allowed(path)
            .iter()
            .all(|sid| *sid == user || sid == "S-1-5-18" || sid == "S-1-5-32-544")
    }
}

/// Makes `path` its owner's alone (`private` then says so).
pub fn make_private(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if path.is_dir() { 0o700 } else { 0o600 };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }
    #[cfg(windows)]
    windows::only_user(path);
}

/// Makes `path` its owner's to read but not to change.
pub fn make_read_only(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if path.is_dir() { 0o500 } else { 0o400 };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }
    #[cfg(windows)]
    windows::only_user_reads(path);
}

/// Makes `path` unreadable, even by its owner, until `make_private`.
pub fn make_unreadable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000)).unwrap();
    }
    #[cfg(windows)]
    windows::only_user_relists(path);
}

/// Makes `path` readable by others too (`private` then says it isn't).
pub fn make_shared(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if path.is_dir() { 0o755 } else { 0o644 };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }
    #[cfg(windows)]
    windows::also_everyone(path);
}

/// Has the processes this test starts share a console with it, on Windows,
/// so `terminate` reaches them: one is made, hidden, if it has none.
pub fn share_console() {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Console::{AllocConsole, GetConsoleWindow};
        use windows_sys::Win32::UI::WindowsAndMessaging::{SW_HIDE, ShowWindow};
        static SHARED: std::sync::Once = std::sync::Once::new();
        SHARED.call_once(|| {
            // SAFETY: plain calls on this process's own console.
            unsafe {
                if GetConsoleWindow().is_null() && AllocConsole() != 0 {
                    ShowWindow(GetConsoleWindow(), SW_HIDE);
                }
            }
        });
    }
}

/// `command` in a process group of its own, so `terminate` asks it alone
/// to stop (Windows' Ctrl-Break goes to a group); nothing on Unix.
pub fn own_group(command: &mut std::process::Command) -> &mut std::process::Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NEW_PROCESS_GROUP);
    }
    command
}

/// Asks process `pid` to stop, as its owner would from outside: SIGTERM,
/// or on Windows Ctrl-Break to its group (`own_group`, `share_console`).
/// On Windows, only for a process `own_group` started: Ctrl-Break to one
/// that leads no group reaches every process on the console, the test
/// runner and cargo among them.
pub fn terminate(pid: u32) {
    #[cfg(unix)]
    // SAFETY: kill sends a signal and touches no memory of ours.
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGTERM);
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, GenerateConsoleCtrlEvent};
        // SAFETY: plain values.
        unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid) };
    }
}

/// A local socket at latch's `name` for it in `state` (`serve-v4`, say)
/// that never answers, held until dropped: a Unix socket at `NAME.sock`,
/// or on Windows a loopback port the `NAME.endpoint` file names.
pub fn deaf_socket(state: &Path, name: &str) -> impl Drop {
    struct Held(#[allow(dead_code)] Box<dyn std::any::Any>, PathBuf);
    impl Drop for Held {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.1);
        }
    }
    #[cfg(unix)]
    {
        let path = state.join(format!("{name}.sock"));
        let socket = std::os::unix::net::UnixListener::bind(&path).unwrap();
        Held(Box::new(socket), path)
    }
    #[cfg(windows)]
    {
        let path = state.join(format!("{name}.endpoint"));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let secret = "00".repeat(32);
        std::fs::write(
            &path,
            format!("{} {secret}\n", listener.local_addr().unwrap()),
        )
        .unwrap();
        Held(Box::new(listener), path)
    }
}

/// The local socket of a host on `state`: a Unix socket, or on Windows the
/// endpoint file that names its loopback port and secret.
pub fn local_socket_path(state: &Path) -> PathBuf {
    state.join(if cfg!(windows) {
        "serve-v4.endpoint"
    } else {
        "serve-v4.sock"
    })
}

#[cfg(unix)]
pub type LocalSocket = std::os::unix::net::UnixStream;
#[cfg(windows)]
pub type LocalSocket = std::net::TcpStream;

/// A connection to the host on `state` over its local socket, as a local
/// caller makes one: on Windows, its half of the handshake done (each end
/// proves it knows the endpoint file's secret; see latch's transport.rs).
pub fn local_socket(state: &Path) -> LocalSocket {
    #[cfg(unix)]
    return LocalSocket::connect(local_socket_path(state)).unwrap();
    #[cfg(windows)]
    {
        use hmac::{Hmac, KeyInit, Mac};
        use std::io::{Read, Write};
        let endpoint = std::fs::read_to_string(local_socket_path(state)).unwrap();
        let (address, secret) = endpoint.trim_end().split_once(' ').unwrap();
        let mut socket = LocalSocket::connect(address).unwrap();
        let proof = |side: &[u8], first: &[u8], second: &[u8]| {
            let mut mac = Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes()).unwrap();
            mac.update(side);
            mac.update(first);
            mac.update(second);
            mac
        };
        let mut host = [0u8; 32];
        socket.read_exact(&mut host).unwrap();
        let mine: [u8; 32] = rand::random();
        let sent = proof(b"latch caller", &host, &mine).finalize().into_bytes();
        socket
            .write_all(&[mine.as_slice(), &sent].concat())
            .unwrap();
        let mut answer = [0u8; 32];
        socket.read_exact(&mut answer).unwrap();
        proof(b"latch host", &mine, &host)
            .verify_slice(&answer)
            .unwrap();
        socket
    }
}

/// Ends process `pid` at once.
pub fn kill(pid: u32) {
    #[cfg(unix)]
    // SAFETY: kill sends a signal and touches no memory of ours.
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGKILL);
    }
    #[cfg(windows)]
    windows::with_process(pid, |process| {
        // SAFETY: a live handle with PROCESS_TERMINATE.
        unsafe { windows_sys::Win32::System::Threading::TerminateProcess(process, 1) };
    });
}

/// Stops process `pid` running until `resume`.
pub fn pause(pid: u32) {
    #[cfg(unix)]
    // SAFETY: kill sends a signal and touches no memory of ours.
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGSTOP);
    }
    #[cfg(windows)]
    windows::with_process(pid, |process| {
        // SAFETY: a live handle with PROCESS_SUSPEND_RESUME.
        unsafe { windows::NtSuspendProcess(process) };
    });
}

/// Lets process `pid`, which `pause` stopped, run again.
pub fn resume(pid: u32) {
    #[cfg(unix)]
    // SAFETY: kill sends a signal and touches no memory of ours.
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGCONT);
    }
    #[cfg(windows)]
    windows::with_process(pid, |process| {
        // SAFETY: a live handle with PROCESS_SUSPEND_RESUME.
        unsafe { windows::NtResumeProcess(process) };
    });
}

/// Whether process `pid` runs.
pub fn alive(pid: u32) -> bool {
    #[cfg(unix)]
    // SAFETY: signal 0 only asks whether the process exists.
    return unsafe { libc::kill(pid as libc::pid_t, 0) } == 0;
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::WAIT_TIMEOUT;
        use windows_sys::Win32::System::Threading::WaitForSingleObject;
        let mut running = false;
        windows::with_process(pid, |process| {
            // SAFETY: a live handle with SYNCHRONIZE.
            running = unsafe { WaitForSingleObject(process, 0) } == WAIT_TIMEOUT;
        });
        running
    }
}

#[cfg(windows)]
mod windows {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use std::ptr::{null, null_mut};

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, EXPLICIT_ACCESS_W, GRANT_ACCESS, GetNamedSecurityInfoW,
        NO_MULTIPLE_TRUSTEE, SE_FILE_OBJECT, SET_ACCESS, SetEntriesInAclW, SetNamedSecurityInfoW,
        TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
    };
    use windows_sys::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_SIZE_INFORMATION, AclSizeInformation,
        DACL_SECURITY_INFORMATION, GetAce, GetAclInformation, GetTokenInformation, NO_INHERITANCE,
        PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
        SUB_CONTAINERS_AND_OBJECTS_INHERIT, TOKEN_QUERY, TOKEN_USER, TokenUser,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ALL_ACCESS, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, READ_CONTROL, WRITE_DAC,
    };
    use windows_sys::Win32::System::SystemServices::ACCESS_ALLOWED_ACE_TYPE;
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_SUSPEND_RESUME,
        PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    };

    #[link(name = "ntdll")]
    unsafe extern "system" {
        pub fn NtSuspendProcess(process: HANDLE) -> i32;
        pub fn NtResumeProcess(process: HANDLE) -> i32;
    }

    pub fn with_process(pid: u32, act: impl FnOnce(HANDLE)) {
        let access = PROCESS_TERMINATE | PROCESS_SUSPEND_RESUME | PROCESS_SYNCHRONIZE;
        // SAFETY: plain values; null is failure.
        let process = unsafe { OpenProcess(access, 0, pid) };
        if !process.is_null() {
            act(process);
            // SAFETY: ours, closed once.
            unsafe { CloseHandle(process) };
        }
    }

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain([0]).collect()
    }

    fn sid_text(sid: PSID) -> String {
        let mut text = null_mut();
        // SAFETY: a valid SID; `text` is freed below.
        unsafe { ConvertSidToStringSidW(sid, &mut text) };
        // SAFETY: a NUL-terminated string from the call above.
        let length = (0..).take_while(|&i| unsafe { *text.add(i) } != 0).count();
        // SAFETY: `length` units are readable.
        let out = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, length) });
        // SAFETY: allocated by ConvertSidToStringSidW.
        unsafe { LocalFree(text.cast()) };
        out
    }

    /// This user's SID, as text.
    pub fn user() -> String {
        let mut token = null_mut();
        // SAFETY: this process's token, query only, closed below.
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) };
        let mut buffer = vec![0u8; 256];
        let mut size = 0;
        // SAFETY: `buffer` is writable for its length.
        unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                buffer.len() as u32,
                &mut size,
            );
            CloseHandle(token);
        }
        // SAFETY: TOKEN_USER starts the buffer GetTokenInformation filled.
        sid_text(unsafe { (*(buffer.as_ptr() as *const TOKEN_USER)).User.Sid })
    }

    /// The SIDs every allowing entry on `path` names, as text.
    pub fn allowed(path: &Path) -> Vec<String> {
        let mut list: *mut ACL = null_mut();
        let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
        // SAFETY: a NUL-terminated name; the descriptor is freed below.
        let read = unsafe {
            GetNamedSecurityInfoW(
                wide(path).as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                &mut list,
                null_mut(),
                &mut descriptor,
            )
        };
        assert_eq!(read, 0, "{}", path.display());
        let mut size = ACL_SIZE_INFORMATION::default();
        // SAFETY: `size` is writable for its own length; `list` is live.
        unsafe {
            GetAclInformation(
                list,
                (&raw mut size).cast(),
                size_of::<ACL_SIZE_INFORMATION>() as u32,
                AclSizeInformation,
            )
        };
        let mut sids = Vec::new();
        for index in 0..size.AceCount {
            let mut ace = null_mut();
            // SAFETY: `index` is below the count.
            unsafe { GetAce(list, index, &mut ace) };
            // SAFETY: every entry starts with a header; an allowed one ends with its SID.
            unsafe {
                if (*(ace as *const ACE_HEADER)).AceType as u32 == ACCESS_ALLOWED_ACE_TYPE {
                    let sid = (&raw mut (*(ace as *mut ACCESS_ALLOWED_ACE)).SidStart).cast();
                    sids.push(sid_text(sid));
                }
            }
        }
        // SAFETY: allocated by GetNamedSecurityInfoW.
        unsafe { LocalFree(descriptor) };
        sids
    }

    fn set(path: &Path, entries: &[(&str, u32)]) {
        let sids: Vec<Vec<u16>> = entries
            .iter()
            .map(|(sid, _)| sid.encode_utf16().chain([0]).collect())
            .collect();
        let mut raw: Vec<PSID> = Vec::new();
        for sid in &sids {
            let mut out = null_mut();
            // SAFETY: a NUL-terminated SID string.
            unsafe {
                windows_sys::Win32::Security::Authorization::ConvertStringSidToSidW(
                    sid.as_ptr(),
                    &mut out,
                )
            };
            raw.push(out);
        }
        let inheritance = if path.is_dir() {
            SUB_CONTAINERS_AND_OBJECTS_INHERIT
        } else {
            NO_INHERITANCE
        };
        let explicit: Vec<EXPLICIT_ACCESS_W> = entries
            .iter()
            .zip(&raw)
            .enumerate()
            .map(|(index, ((_, access), sid))| EXPLICIT_ACCESS_W {
                grfAccessPermissions: *access,
                grfAccessMode: if index == 0 { SET_ACCESS } else { GRANT_ACCESS },
                grfInheritance: inheritance,
                Trustee: TRUSTEE_W {
                    pMultipleTrustee: null_mut(),
                    MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
                    TrusteeForm: TRUSTEE_IS_SID,
                    TrusteeType: TRUSTEE_IS_UNKNOWN,
                    ptstrName: (*sid).cast(),
                },
            })
            .collect();
        let mut list = null_mut();
        // SAFETY: live entries, no old list; `list` is freed below.
        let made = unsafe {
            SetEntriesInAclW(explicit.len() as u32, explicit.as_ptr(), null(), &mut list)
        };
        assert_eq!(made, 0);
        // SAFETY: a NUL-terminated name and a live list.
        let written = unsafe {
            SetNamedSecurityInfoW(
                wide(path).as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                list,
                null(),
            )
        };
        assert_eq!(written, 0, "{}", path.display());
        // SAFETY: each freed once.
        unsafe {
            LocalFree(list.cast());
            for sid in raw {
                LocalFree(sid);
            }
        }
    }

    pub fn only_user(path: &Path) {
        set(path, &[(&user(), FILE_ALL_ACCESS)]);
    }

    pub fn only_user_relists(path: &Path) {
        // Its list read and changed back, nothing else.
        set(path, &[(&user(), READ_CONTROL | WRITE_DAC)]);
    }

    pub fn only_user_reads(path: &Path) {
        // Read and run, and change the list back (WRITE_DAC), nothing else.
        let read = FILE_GENERIC_READ | FILE_GENERIC_EXECUTE | WRITE_DAC;
        set(path, &[(&user(), read)]);
    }

    pub fn also_everyone(path: &Path) {
        set(
            path,
            &[(&user(), FILE_ALL_ACCESS), ("S-1-1-0", FILE_GENERIC_READ)],
        );
    }
}

/// A program at a terminal of its own, as a person runs one: on Unix a pty
/// that is its controlling terminal; on Windows a pseudoconsole. Its stdin
/// and stderr are the terminal, and its stdout too unless piped (then
/// `Child::stdout`). Answers the program, the terminal's keyboard, and what
/// the terminal shows; dropping the keyboard hangs the terminal up, and
/// what it shows then ends.
/// `size` is columns by rows; with none, a Unix pty has no size (what
/// `openpty` gives), and a Windows console one wide enough that no line wraps
/// and long enough that none scrolls.
pub fn terminal(
    command: &mut std::process::Command,
    size: Option<(u16, u16)>,
    pipe_stdout: bool,
) -> (Child, Typing, std::sync::mpsc::Receiver<String>) {
    #[cfg(unix)]
    return unix_terminal(command, size, pipe_stdout);
    #[cfg(windows)]
    return console::terminal(command, size.unwrap_or((250, 3000)), pipe_stdout);
}

/// A program at a `terminal`, as `std::process::Child` answers.
pub struct Child {
    pub stdout: Option<std::fs::File>,
    #[cfg(unix)]
    child: std::process::Child,
    #[cfg(windows)]
    process: console::Process,
}

/// How a program ended: its exit code, none for a signal's end.
#[derive(Debug, PartialEq, Clone, Copy)]
pub struct Status(Option<i32>);

impl Status {
    pub fn code(&self) -> Option<i32> {
        self.0
    }

    pub fn success(&self) -> bool {
        self.0 == Some(0)
    }
}

impl Child {
    pub fn id(&self) -> u32 {
        #[cfg(unix)]
        return self.child.id();
        #[cfg(windows)]
        return self.process.id;
    }

    pub fn try_wait(&mut self) -> std::io::Result<Option<Status>> {
        #[cfg(unix)]
        return Ok(self.child.try_wait()?.map(|status| Status(status.code())));
        #[cfg(windows)]
        return Ok(self.process.try_wait().map(Status));
    }

    pub fn wait(&mut self) -> std::io::Result<Status> {
        #[cfg(unix)]
        return Ok(Status(self.child.wait()?.code()));
        #[cfg(windows)]
        return Ok(Status(self.process.wait()));
    }

    pub fn kill(&mut self) -> std::io::Result<()> {
        #[cfg(unix)]
        return self.child.kill();
        #[cfg(windows)]
        {
            self.process.kill();
            Ok(())
        }
    }
}

/// What a terminal showed with its line breaks out: a console breaks what
/// doesn't fit its width, moving its cursor to do so.
pub fn unbroken(shown: &str) -> String {
    let shown = shown.replace("\r\n", "");
    let mut pieces = shown.split("\x1b[");
    let mut whole = pieces.next().unwrap_or_default().to_string();
    for piece in pieces {
        match piece.split_once('H') {
            Some((at, rest)) if at.chars().all(|c| c.is_ascii_digit() || c == ';') => {
                whole.push_str(rest);
            }
            _ => {
                whole.push_str("\x1b[");
                whole.push_str(piece);
            }
        }
    }
    whole
}

/// A terminal's keyboard: what is written is typed there, `\n` as Enter.
pub struct Typing {
    keys: std::fs::File,
    #[cfg(windows)]
    _console: console::Console,
}

impl Typing {
    pub fn write_all(&mut self, typed: &[u8]) -> std::io::Result<()> {
        use std::io::Write;
        if cfg!(windows) {
            let typed: Vec<u8> = typed
                .iter()
                .map(|&b| if b == b'\n' { b'\r' } else { b })
                .collect();
            return self.keys.write_all(&typed);
        }
        self.keys.write_all(typed)
    }
}

#[cfg(unix)]
fn unix_terminal(
    command: &mut std::process::Command,
    size: Option<(u16, u16)>,
    pipe_stdout: bool,
) -> (Child, Typing, std::sync::mpsc::Receiver<String>) {
    use std::io::Read;
    use std::os::fd::FromRawFd;
    use std::os::unix::process::CommandExt;
    let (mut main, mut side) = (0, 0);
    let size = size.map(|(columns, rows)| libc::winsize {
        ws_row: rows,
        ws_col: columns,
        ws_xpixel: 0,
        ws_ypixel: 0,
    });
    let size = size
        .as_ref()
        .map_or(std::ptr::null(), |size| size as *const libc::winsize);
    // SAFETY: openpty fills in two new descriptors, which we then own; the
    // size, when given, is read.
    let opened = unsafe {
        libc::openpty(
            &mut main,
            &mut side,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            size as _,
        )
    };
    assert_eq!(opened, 0);
    // Neither end may leak into another child: one holding the terminal
    // open would keep it from ever going away.
    for fd in [main, side] {
        // SAFETY: sets close-on-exec on descriptors we own.
        let set = unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
        assert_eq!(set, 0);
    }
    // SAFETY: both descriptors are ours, owned by the Files from here on.
    let (main, side) = unsafe {
        (
            std::fs::File::from_raw_fd(main),
            std::fs::File::from_raw_fd(side),
        )
    };
    command
        .stdin(side.try_clone().unwrap())
        .stderr(side.try_clone().unwrap());
    if pipe_stdout {
        command.stdout(std::process::Stdio::piped());
    } else {
        command.stdout(side.try_clone().unwrap());
    }
    // SAFETY: setsid and the ioctl only change this child's session and
    // terminal, between fork and exec.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().unwrap();
    // Every copy of the terminal's side must close, the command's too, or
    // reading the other side never ends once the program does.
    drop(side);
    let stdout = child.stdout.take().map(|piped| {
        use std::os::fd::{FromRawFd, IntoRawFd};
        // SAFETY: the pipe's descriptor, handed over whole.
        unsafe { std::fs::File::from_raw_fd(piped.into_raw_fd()) }
    });
    let mut reading = main.try_clone().unwrap();
    let (send, shown) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buffer = [0; 4096];
        // Ends with an error once the last holder of the other side exits.
        while let Ok(read) = reading.read(&mut buffer) {
            if read == 0
                || send
                    .send(String::from_utf8_lossy(&buffer[..read]).to_string())
                    .is_err()
            {
                break;
            }
        }
    });
    (Child { stdout, child }, Typing { keys: main }, shown)
}

/// Windows' terminal for a program: a pseudoconsole, its input and output
/// over pipes; and the program's process.
#[cfg(windows)]
mod console {
    use std::ffi::OsStr;
    use std::io::Read;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::FromRawHandle;
    use std::ptr::{null, null_mut};

    use windows_sys::Win32::Foundation::{
        CloseHandle, HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation, WAIT_OBJECT_0,
    };
    use windows_sys::Win32::System::Console::{
        COORD, ClosePseudoConsole, CreatePseudoConsole, HPCON,
    };
    use windows_sys::Win32::System::Pipes::CreatePipe;
    use windows_sys::Win32::System::Threading::{
        CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList,
        EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, INFINITE,
        InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
        PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
        PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess,
        UpdateProcThreadAttribute, WaitForSingleObject,
    };

    use super::{Child, Typing};

    /// A pseudoconsole, closed when dropped: what it shows then ends.
    pub struct Console(HPCON);

    impl Drop for Console {
        fn drop(&mut self) {
            // SAFETY: ours, closed once.
            unsafe { ClosePseudoConsole(self.0) };
        }
    }

    pub struct Process {
        handle: HANDLE,
        pub id: u32,
    }

    // SAFETY: a process handle may be used from any thread.
    unsafe impl Send for Process {}

    impl Process {
        fn code(&self) -> Option<i32> {
            let mut code = 0u32;
            // SAFETY: a live handle; `code` is writable.
            unsafe { GetExitCodeProcess(self.handle, &mut code) };
            Some(code as i32)
        }

        pub fn try_wait(&mut self) -> Option<Option<i32>> {
            // SAFETY: a live handle.
            match unsafe { WaitForSingleObject(self.handle, 0) } {
                WAIT_OBJECT_0 => Some(self.code()),
                _ => None,
            }
        }

        pub fn wait(&mut self) -> Option<i32> {
            // SAFETY: a live handle.
            unsafe { WaitForSingleObject(self.handle, INFINITE) };
            self.code()
        }

        pub fn kill(&mut self) {
            // SAFETY: a live handle.
            unsafe { TerminateProcess(self.handle, 1) };
        }
    }

    impl Drop for Process {
        fn drop(&mut self) {
            // SAFETY: ours, closed once.
            unsafe { CloseHandle(self.handle) };
        }
    }

    fn pipe() -> (HANDLE, HANDLE) {
        let (mut read, mut write) = (null_mut(), null_mut());
        // SAFETY: two handles written on success, not inheritable.
        assert_ne!(unsafe { CreatePipe(&mut read, &mut write, null(), 0) }, 0);
        (read, write)
    }

    /// One word of a command line, as Windows' C runtime splits it again.
    fn quoted(word: &OsStr) -> Vec<u16> {
        let text: Vec<u16> = word.encode_wide().collect();
        let plain = !text.is_empty()
            && !text
                .iter()
                .any(|&c| c == b' ' as u16 || c == b'\t' as u16 || c == b'"' as u16);
        if plain {
            return text;
        }
        let mut out = vec![b'"' as u16];
        let mut slashes = 0;
        for &c in &text {
            if c == b'\\' as u16 {
                slashes += 1;
                continue;
            }
            let doubled = if c == b'"' as u16 {
                slashes * 2 + 1
            } else {
                slashes
            };
            out.extend(std::iter::repeat_n(b'\\' as u16, doubled));
            out.push(c);
            slashes = 0;
        }
        out.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
        out.push(b'"' as u16);
        out
    }

    /// What a program wrote, out of what its console sent; an unfinished
    /// sequence or character stays in `unread`. A console draws a screen
    /// rather than pass a stream on: it sets itself up, titles its window,
    /// moves past a space instead of writing it, and numbers each link,
    /// none of which the program wrote.
    fn written(unread: &mut Vec<u8>) -> String {
        const ITS_OWN: &[&[u8]] = &[
            b"?9001h", b"?9001l", b"?1004h", b"?1004l", b"?25h", b"?25l", b"2J", b"H", b"J", b"K",
            b"m",
        ];
        let mut text = Vec::new();
        let mut at = 0;
        while at < unread.len() {
            if unread[at] != 0x1b {
                text.push(unread[at]);
                at += 1;
                continue;
            }
            let rest = &unread[at + 1..];
            let length = match rest.first() {
                None => None,
                Some(b'[') => rest
                    .iter()
                    .skip(1)
                    .position(|byte| (0x40..=0x7e).contains(byte))
                    .map(|end| end + 2),
                Some(b']') => {
                    (1..rest.len()).find_map(|end| match (rest[end], rest.get(end + 1)) {
                        (7, _) => Some(end + 1),
                        (0x1b, Some(b'\\')) => Some(end + 2),
                        _ => None,
                    })
                }
                Some(_) => Some(1),
            };
            let Some(length) = length else { break };
            let sequence = &rest[..length];
            at += 1 + length;
            match sequence {
                [b'[', moved @ .., b'C'] => {
                    let moved = std::str::from_utf8(moved).ok().and_then(|n| n.parse().ok());
                    text.resize(text.len() + moved.unwrap_or(1), b' ');
                }
                [b'[', .., b';', b'1', b'H'] => text.extend_from_slice(b"\r\n"),
                [b'[', own @ ..] if ITS_OWN.contains(&own) => {}
                [b']', b'0', b';', ..] => {}
                [b']', b'8', b';', b'i', b'd', b'=', link @ ..] => {
                    let link = link
                        .iter()
                        .position(|byte| *byte == b';')
                        .map_or(link, |id| &link[id..]);
                    text.extend_from_slice(b"\x1b]8;");
                    text.extend_from_slice(link);
                }
                _ => {
                    text.push(0x1b);
                    text.extend_from_slice(sequence);
                }
            }
        }
        unread.drain(..at);
        if let Err(cut) = std::str::from_utf8(&text)
            && cut.error_len().is_none()
        {
            unread.splice(0..0, text.split_off(cut.valid_up_to()));
        }
        String::from_utf8_lossy(&text).to_string()
    }

    pub fn terminal(
        command: &mut std::process::Command,
        (columns, rows): (u16, u16),
        pipe_stdout: bool,
    ) -> (Child, Typing, std::sync::mpsc::Receiver<String>) {
        let (input_read, input_write) = pipe();
        let (output_read, output_write) = pipe();
        let mut console: HPCON = 0;
        let size = COORD {
            X: columns as i16,
            Y: rows as i16,
        };
        // SAFETY: two live pipe ends; `console` written on success.
        let made = unsafe { CreatePseudoConsole(size, input_read, output_write, 0, &mut console) };
        assert_eq!(made, 0);
        // The pseudoconsole holds its own copies.
        // SAFETY: ours, closed once.
        unsafe {
            CloseHandle(input_read);
            CloseHandle(output_write);
        }
        let (stdout_read, mut stdout_write) = (null_mut(), null_mut());
        let stdout_read = if pipe_stdout {
            let (read, write) = pipe();
            // SAFETY: the write end, made inheritable for the child alone (the handle list below).
            unsafe { SetHandleInformation(write, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) };
            stdout_write = write;
            read
        } else {
            stdout_read
        };

        // The command line, environment and folder `command` names.
        let mut line: Vec<u16> = quoted(command.get_program());
        for arg in command.get_args() {
            line.push(b' ' as u16);
            line.extend(quoted(arg));
        }
        line.push(0);
        let mut vars: Vec<(std::ffi::OsString, std::ffi::OsString)> = std::env::vars_os().collect();
        for (name, value) in command.get_envs() {
            vars.retain(|(have, _)| !have.eq_ignore_ascii_case(name));
            if let Some(value) = value {
                vars.push((name.to_os_string(), value.to_os_string()));
            }
        }
        vars.sort_by_key(|(name, _)| name.to_ascii_uppercase());
        let mut environment: Vec<u16> = Vec::new();
        for (name, value) in &vars {
            environment.extend(name.encode_wide());
            environment.push(b'=' as u16);
            environment.extend(value.encode_wide());
            environment.push(0);
        }
        environment.push(0);
        let folder: Option<Vec<u16>> = command
            .get_current_dir()
            .map(|dir| dir.as_os_str().encode_wide().chain([0]).collect());

        let mut size = 0;
        // SAFETY: asks the size for two attributes.
        unsafe { InitializeProcThreadAttributeList(null_mut(), 2, 0, &mut size) };
        let mut list = vec![0u8; size];
        let attributes = list.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
        // SAFETY: `list` is `size` bytes, live until deleted below.
        let ready = unsafe { InitializeProcThreadAttributeList(attributes, 2, 0, &mut size) };
        assert_ne!(ready, 0);
        // SAFETY: the pseudoconsole's value itself, as this attribute takes it.
        let given = unsafe {
            UpdateProcThreadAttribute(
                attributes,
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                console as *const _,
                size_of::<HPCON>(),
                null_mut(),
                null(),
            )
        };
        assert_ne!(given, 0);
        let inherited = [stdout_write];
        if pipe_stdout {
            // SAFETY: the one handle the child inherits, live meanwhile.
            let given = unsafe {
                UpdateProcThreadAttribute(
                    attributes,
                    0,
                    PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                    inherited.as_ptr().cast(),
                    size_of_val(&inherited),
                    null_mut(),
                    null(),
                )
            };
            assert_ne!(given, 0);
        }
        // SAFETY: zeroed is a valid STARTUPINFOEXW once its size is set.
        let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
        startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        startup.lpAttributeList = attributes;
        // Its own: with none named, it would take this process's, and a test
        // run with its output redirected would keep the program off the
        // console. Stdout to the pipe, if any; what is left null is the console's.
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        if pipe_stdout {
            startup.StartupInfo.hStdOutput = stdout_write;
        }
        // SAFETY: zeroed is a valid PROCESS_INFORMATION to receive into.
        let mut process: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: every buffer is live and NUL-terminated for the call.
        let started = unsafe {
            CreateProcessW(
                null(),
                line.as_mut_ptr(),
                null(),
                null(),
                pipe_stdout as i32,
                EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
                environment.as_ptr().cast(),
                folder.as_ref().map_or(null(), |dir| dir.as_ptr()),
                &startup.StartupInfo,
                &mut process,
            )
        };
        assert_ne!(started, 0, "{}", std::io::Error::last_os_error());
        // SAFETY: the list, deleted once; the thread handle and our copy of
        // the child's stdout, closed once.
        unsafe {
            DeleteProcThreadAttributeList(attributes);
            CloseHandle(process.hThread);
            if pipe_stdout {
                CloseHandle(stdout_write);
            }
        }
        // SAFETY: each handle is ours, owned by its File from here on.
        let (typing, mut reading, stdout) = unsafe {
            (
                std::fs::File::from_raw_handle(input_write),
                std::fs::File::from_raw_handle(output_read),
                pipe_stdout.then(|| std::fs::File::from_raw_handle(stdout_read)),
            )
        };
        let (send, shown) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut buffer = [0; 4096];
            let mut unread = Vec::new();
            // Ends once the pseudoconsole is closed.
            while let Ok(read) = reading.read(&mut buffer) {
                unread.extend_from_slice(&buffer[..read]);
                if read == 0 || send.send(written(&mut unread)).is_err() {
                    break;
                }
            }
        });
        let child = Child {
            stdout,
            process: Process {
                handle: process.hProcess,
                id: process.dwProcessId,
            },
        };
        (
            child,
            Typing {
                keys: typing,
                _console: Console(console),
            },
            shown,
        )
    }
}

//! The Windows worker sandbox: an AppContainer inside a Job object.
//!
//! Windows cannot confine a running process, so `confine` works as Linux's
//! does: the first copy of the worker is a launcher, outside any sandbox, and
//! starts the same worker again inside one, then waits for it and ends as it
//! ended. The launcher makes an AppContainer profile of the call's own, so no
//! two calls share what they were granted, and gives that container exactly
//! the policy's paths: its areas and temp folder to read and write, its reads
//! in home (and the latch it runs) to read. Outside home it reads what any
//! AppContainer may, the system's own folders. A hidden path inside one gets an
//! entry that denies the container, which wins over any grant. A container
//! reaches nothing else of the user's, and no network unless the policy
//! grants it; loopback is closed to it, so the host's local socket is too.
//!
//! Unlike macOS and Linux, an `exec` worker does not read all of home here:
//! that would mean an entry on every file in the profile. It reads its areas
//! and what the policy names; home-wide reading is left for an opt-in.
//!
//! The worker runs in a Job object that kills everything in it when its last
//! handle closes: the launcher holds it, so a launcher that ends, however it
//! ends, takes the worker and every program it started with it. `kill_after`
//! is the launcher's: it ends the job then. The host keeps every launcher in a
//! job of its own (`tie_to_host`), so a host that dies takes them all.
//!
//! The grants are written into the files' own access lists, so the launcher
//! takes them back when the worker ends: grants and denials alike. It first
//! writes what it grants to a ledger in the state folder (not the temp
//! folder, which for a launcher is the call's own and goes with it); a
//! launcher that was killed leaves its ledger, and the host takes those
//! grants back (`sweep`): when it kills a launcher, and when it starts.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_SUCCESS, HANDLE, HANDLE_FLAG_INHERIT, LocalFree, SetHandleInformation,
    WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
    ConvertStringSidToSidW, DENY_ACCESS, EXPLICIT_ACCESS_W, GRANT_ACCESS, GetSecurityInfo,
    NO_MULTIPLE_TRUSTEE, SDDL_REVISION_1, SE_FILE_OBJECT, SetEntriesInAclW, SetNamedSecurityInfoW,
    TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
};
use windows_sys::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeleteAppContainerProfile, DeriveAppContainerSidFromAppContainerName,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_SIZE_INFORMATION, AclSizeInformation, AddAce,
    DACL_SECURITY_INFORMATION, EqualSid, FreeSid, GetAce, GetAclInformation,
    GetSecurityDescriptorControl, GetTokenInformation, INHERITED_ACE, InitializeAcl,
    InitializeSecurityDescriptor, NO_INHERITANCE, PSECURITY_DESCRIPTOR, PSID,
    SE_DACL_AUTO_INHERITED, SE_DACL_PROTECTED, SECURITY_ATTRIBUTES, SECURITY_CAPABILITIES,
    SECURITY_DESCRIPTOR, SID_AND_ATTRIBUTES, SUB_CONTAINERS_AND_OBJECTS_INHERIT,
    SetKernelObjectSecurity, SetSecurityDescriptorControl, SetSecurityDescriptorDacl, TOKEN_QUERY,
    TOKEN_USER, TokenIsAppContainer, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    DELETE, FILE_FLAG_BACKUP_SEMANTICS, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ,
    FILE_GENERIC_WRITE, FILE_READ_ATTRIBUTES, FILE_READ_DATA, FILE_SHARE_READ, FILE_SHARE_WRITE,
    FILE_TRAVERSE, GetFinalPathNameByHandleW, READ_CONTROL, SYNCHRONIZE, WRITE_DAC,
};
use windows_sys::Win32::System::Console::{
    GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::Environment::GetCommandLineW;
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectExtendedLimitInformation, SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::StationsAndDesktops::{
    CloseDesktop, CloseWindowStation, CreateDesktopW, CreateWindowStationW,
    GetProcessWindowStation, GetUserObjectInformationW, HDESK, HWINSTA, SetProcessWindowStation,
    UOI_FLAGS, USEROBJECTFLAGS,
};
use windows_sys::Win32::System::SystemServices::{
    ACCESS_ALLOWED_ACE_TYPE, ACCESS_DENIED_ACE_TYPE, MAXDWORD, SE_GROUP_ENABLED,
    SECURITY_DESCRIPTOR_REVISION,
};
use windows_sys::Win32::System::Threading::{
    CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateMutexW, CreateProcessW,
    DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess,
    GetExitCodeProcess, INFINITE, InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
    OpenProcess, OpenProcessToken, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, PROCESS_INFORMATION, PROCESS_SYNCHRONIZE,
    ReleaseMutex, ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess,
    UpdateProcThreadAttribute, WaitForSingleObject,
};

use super::Policy;

/// Set in the worker the launcher starts: it is inside its container.
const INNER: &str = "LATCH_SANDBOX_INNER";

/// What may run a program and read: a file's read, its traverse and its run.
const READ: u32 = FILE_GENERIC_READ | FILE_GENERIC_EXECUTE;
/// An area's: that, write, and delete what is in it.
const WRITE: u32 = READ | FILE_GENERIC_WRITE | DELETE;
/// A folder a program runs in: entered, nothing in it listed or read.
const ENTER: u32 = FILE_TRAVERSE | FILE_READ_ATTRIBUTES | SYNCHRONIZE;

/// The capability a container needs for outbound connections.
const INTERNET_CLIENT: &str = "S-1-15-3-1";

/// AppContainers are part of Windows since 8; there is nothing to probe
/// before the probe worker confines itself for real.
pub fn check() -> Result<(), String> {
    Ok(())
}

pub struct HostWatch;

/// The launcher outside the container watches the host (`tie_to_host`).
pub fn watch_host() -> Result<HostWatch, String> {
    Ok(HostWatch)
}

/// The launcher ends the job at `kill_after`; nothing to do here.
pub fn supervise(_watch: HostWatch, _kill_after: Option<u64>) -> Result<(), String> {
    Ok(())
}

pub fn confine(policy: &Policy) -> Result<(), String> {
    if std::env::var_os(INNER).is_some() {
        return match in_container() {
            true => Ok(()),
            false => Err("this worker should be in its container and is not".to_string()),
        };
    }
    if policy.break_for_test {
        return Err("the sandbox was broken on purpose, for a test".to_string());
    }
    let code = launch(policy)?;
    std::process::exit(code as i32);
}

/// Whether this process runs in an AppContainer.
fn in_container() -> bool {
    let mut token: HANDLE = null_mut();
    // SAFETY: a query-only token of this process, closed below.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return false;
    }
    let mut is_container = 0u32;
    let mut size = 0u32;
    // SAFETY: `is_container` is writable for the u32 this class answers.
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenIsAppContainer,
            (&mut is_container as *mut u32).cast(),
            size_of::<u32>() as u32,
            &mut size,
        )
    };
    // SAFETY: the token is ours and closed once.
    unsafe { CloseHandle(token) };
    ok != 0 && is_container != 0
}

/// One grant: a path, and what the container may do there.
#[derive(Serialize, Deserialize)]
struct Grant {
    path: PathBuf,
    access: Access,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq)]
enum Access {
    /// Read it, and everything below a folder.
    Read,
    /// Read and write it, and everything below a folder.
    Write,
    /// Nothing, whatever a grant above it says.
    Deny,
    /// Enter a folder (the one a program runs in), nothing in it listed or read.
    Enter,
}

/// What a launcher granted, kept until it has taken it all back.
#[derive(Serialize, Deserialize)]
struct Ledger {
    container: String,
    launcher: u32,
    grants: Vec<Grant>,
}

/// Where a host on `state_dir` keeps its launchers' ledgers.
fn ledgers(state_dir: &Path) -> PathBuf {
    state_dir.join("sandbox")
}

/// Starts this worker again in a container of its own and waits for it.
fn launch(policy: &Policy) -> Result<u32, String> {
    let container = format!("latch.worker.{:016x}", rand::random::<u64>());
    let grants = grants(policy)?;
    let ledger = Ledger {
        container: container.clone(),
        launcher: std::process::id(),
        grants,
    };
    let folder = ledgers(&policy.state_dir);
    let ledger_file = folder.join(format!("{container}.json"));
    std::fs::create_dir_all(&folder)
        .and_then(|()| {
            std::fs::write(
                &ledger_file,
                serde_json::to_vec(&ledger).expect("a ledger serializes"),
            )
        })
        .map_err(|err| format!("{}: {err}", ledger_file.display()))?;
    let sid = Sid::container(&container)?;
    let result = grant_all(&sid, &ledger.grants).and_then(|()| run_inside(&sid, policy));
    take_back(&sid, &ledger.grants);
    drop(sid);
    // SAFETY: a NUL-terminated name of this launcher's own profile.
    unsafe { DeleteAppContainerProfile(wide(OsStr::new(&container)).as_ptr()) };
    let _ = std::fs::remove_file(&ledger_file);
    result
}

/// The policy's paths, as grants: what exists of them, hidden paths inside
/// a granted one denied.
fn grants(policy: &Policy) -> Result<Vec<Grant>, String> {
    let program =
        std::env::current_exe().map_err(|err| format!("latch cannot find itself: {err}"))?;
    let mut grants = Vec::new();
    for area in policy.areas.iter().chain(&policy.tmpdir) {
        if area.exists() {
            grants.push(Grant {
                path: area.clone(),
                access: Access::Write,
            });
        }
    }
    // Outside home a worker reads only what any AppContainer may (the
    // system's own folders), as on macOS and Linux it reads the system's
    // files and nothing else there: a get never opens another place up.
    let home = std::env::home_dir().filter(|home| home.is_absolute());
    let real_home = home.as_ref().and_then(|home| home.canonicalize().ok());
    let in_home = |path: &Path| {
        [&home, &real_home]
            .into_iter()
            .flatten()
            .any(|home| path.starts_with(home))
    };
    let reads = policy
        .reads
        .iter()
        .chain(&policy.program)
        .filter(|read| in_home(read));
    // The latch it runs, wherever it is.
    for read in reads.chain(std::iter::once(&program)) {
        if read.exists() {
            grants.push(Grant {
                path: read.clone(),
                access: Access::Read,
            });
        }
    }
    let hidden = policy
        .hidden
        .iter()
        .chain(std::iter::once(&policy.state_dir));
    let granted: Vec<PathBuf> = grants.iter().map(|grant| grant.path.clone()).collect();
    for path in hidden {
        if path.exists() && granted.iter().any(|open| path.starts_with(open)) {
            grants.push(Grant {
                path: path.clone(),
                access: Access::Deny,
            });
        }
    }
    // The folder it runs in, when no grant reaches it: entered, nothing in
    // it listed or read.
    if let Some(dir) = &policy.workdir
        && dir.is_dir()
        && !granted.iter().any(|open| dir.starts_with(open))
    {
        grants.push(Grant {
            path: dir.clone(),
            access: Access::Enter,
        });
    }
    Ok(grants)
}

fn grant_all(sid: &Sid, grants: &[Grant]) -> Result<(), String> {
    for grant in grants {
        set_entry(grant, sid.0).map_err(|err| {
            format!(
                "{}: cannot grant the sandbox access: {err}",
                grant.path.display()
            )
        })?;
    }
    Ok(())
}

/// Access-list edits, one at a time across this user's launchers. Each is
/// a read, a change and a write of the whole list, so two at once write
/// over each other: a grant lost, or an entry taken back that comes back.
/// The latch it runs and any area two calls share are edited by both. A
/// launcher killed while holding it abandons it, which frees it; a
/// container cannot reach it (its `Local` names are its own).
struct EditLock(HANDLE);

impl EditLock {
    fn take() -> std::io::Result<EditLock> {
        let name = wide(OsStr::new("Local\\latch.sandbox.access-lists"));
        // SAFETY: a NUL-terminated name, no security attributes.
        let mutex = unsafe { CreateMutexW(null(), 0, name.as_ptr()) };
        if mutex.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: a live handle of ours.
        match unsafe { WaitForSingleObject(mutex, INFINITE) } {
            WAIT_OBJECT_0 | WAIT_ABANDONED => Ok(EditLock(mutex)),
            _ => {
                let err = std::io::Error::last_os_error();
                // SAFETY: closed once, here.
                unsafe { CloseHandle(mutex) };
                Err(err)
            }
        }
    }
}

impl Drop for EditLock {
    fn drop(&mut self) {
        // SAFETY: held by this thread since `take`, released and closed once.
        unsafe {
            ReleaseMutex(self.0);
            CloseHandle(self.0);
        }
    }
}

/// Takes back every entry for the container: grants and denials alike.
/// One that is gone, or no longer at its path, keeps an entry for a
/// container no process can run in again.
fn take_back(sid: &Sid, grants: &[Grant]) {
    for grant in grants {
        let _ = with_dacl(
            &grant.path,
            grant.access != Access::Enter,
            |old, _, write| {
                // SAFETY: `old` is the object's access list, live meanwhile.
                match unsafe { without(old, sid.0) }? {
                    Some(mut new) => write(new.as_mut_ptr().cast()),
                    None => Ok(()),
                }
            },
        );
    }
}

/// The object at a judged path, held open while its access list is
/// written: neither it nor a folder above it can be renamed meanwhile, so
/// its name cannot be pointed elsewhere (a folder swapped for a junction
/// into latch's state). Opened through any links, it must be that path.
struct Pin {
    file: std::fs::File,
    folder: bool,
}

impl Pin {
    fn open(path: &Path) -> std::io::Result<Pin> {
        use std::os::windows::fs::OpenOptionsExt;
        // Reading its data, not only its attributes, is what makes Windows
        // check the sharing: without delete, no rename.
        let file = std::fs::OpenOptions::new()
            .access_mode(
                FILE_READ_DATA | FILE_READ_ATTRIBUTES | READ_CONTROL | WRITE_DAC | SYNCHRONIZE,
            )
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)?;
        if !same(&final_path(&file)?, path) {
            return Err(std::io::Error::other(
                "it is not what was judged at that path",
            ));
        }
        let folder = file.metadata()?.is_dir();
        Ok(Pin { file, folder })
    }
}

/// The same path, apart from a `\\?\` prefix and case.
fn same(a: &Path, b: &Path) -> bool {
    let plain = |path: &Path| {
        let text = path.to_string_lossy();
        text.strip_prefix(r"\\?\").unwrap_or(&text).to_lowercase()
    };
    plain(a) == plain(b)
}

/// Where the open `file` really is, as `canonicalize` names it.
pub(crate) fn final_path(file: &std::fs::File) -> std::io::Result<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    let mut buffer = vec![0u16; 1024];
    // SAFETY: `buffer` is writable for its length; the handle is live.
    let length = unsafe {
        GetFinalPathNameByHandleW(
            file.as_raw_handle(),
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            0,
        )
    } as usize;
    if length == 0 || length > buffer.len() {
        return Err(std::io::Error::last_os_error());
    }
    Ok(PathBuf::from(std::ffi::OsString::from_wide(
        &buffer[..length],
    )))
}

/// Writes the access list of the object at `path` again, from its current
/// one: `edit` gets it, whether the object is a folder, and the writer for
/// the list it makes. One edit at a time (`EditLock`), the object pinned
/// (`Pin`). A folder's inherited entries are written by name, so Windows
/// carries them down to everything below it (a walk of the whole folder);
/// anything else through the handle, which walks nothing.
fn with_dacl(
    path: &Path,
    inherited: bool,
    edit: impl FnOnce(*mut ACL, bool, &dyn Fn(*mut ACL) -> std::io::Result<()>) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let _one_at_a_time = EditLock::take()?;
    let pin = Pin::open(path)?;
    let handle = pin.file.as_raw_handle();
    let mut old: *mut ACL = null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: a live handle with READ_CONTROL; the descriptor holding `old`
    // is freed below with LocalFree.
    let read = unsafe {
        GetSecurityInfo(
            handle,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            &mut old,
            null_mut(),
            &mut descriptor,
        )
    };
    if read != ERROR_SUCCESS {
        return Err(std::io::Error::from_raw_os_error(read as i32));
    }
    let name = wide(path.as_os_str());
    let by_name = inherited && pin.folder;
    let write = |new: *mut ACL| {
        if by_name {
            // SAFETY: `name` and `new` are live; no owner, group or SACL change.
            match unsafe {
                SetNamedSecurityInfoW(
                    name.as_ptr(),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION,
                    null_mut(),
                    null_mut(),
                    new,
                    null(),
                )
            } {
                ERROR_SUCCESS => Ok(()),
                err => Err(std::io::Error::from_raw_os_error(err as i32)),
            }
        } else {
            // SAFETY: the pinned handle has WRITE_DAC; `descriptor` is live.
            unsafe { write_through(handle, descriptor, new) }
        }
    };
    let result = edit(old, pin.folder, &write);
    // SAFETY: allocated by GetSecurityInfo, freed once.
    unsafe { LocalFree(descriptor) };
    result
}

/// Sets `new` as the access list of the object open at `handle`, as
/// protected and auto-inherited as `like` says its old one was.
///
/// # Safety
/// `handle` must be live with WRITE_DAC, `like` a live descriptor, `new` a
/// live access list.
unsafe fn write_through(
    handle: HANDLE,
    like: PSECURITY_DESCRIPTOR,
    new: *mut ACL,
) -> std::io::Result<()> {
    let (mut control, mut revision) = (0u16, 0u32);
    let mut descriptor: SECURITY_DESCRIPTOR = unsafe { std::mem::zeroed() };
    let built = (&raw mut descriptor).cast();
    let kept = SE_DACL_PROTECTED | SE_DACL_AUTO_INHERITED;
    // SAFETY: as the caller promised; `descriptor` is ours and outlives the
    // calls.
    let done = unsafe {
        GetSecurityDescriptorControl(like, &mut control, &mut revision) != 0
            && InitializeSecurityDescriptor(built, SECURITY_DESCRIPTOR_REVISION) != 0
            && SetSecurityDescriptorDacl(built, 1, new, 0) != 0
            && SetSecurityDescriptorControl(built, kept, control & kept) != 0
            && SetKernelObjectSecurity(handle, DACL_SECURITY_INFORMATION, built) != 0
    };
    if !done {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// A copy of the access list `old` without its own entries for `sid`, or
/// None when it has none.
///
/// # Safety
/// `old` must be null or a live access list.
unsafe fn without(old: *const ACL, sid: PSID) -> std::io::Result<Option<Vec<u32>>> {
    if old.is_null() {
        return Ok(None);
    }
    let mut size = ACL_SIZE_INFORMATION::default();
    // SAFETY: `size` is writable for its own length.
    let sized = unsafe {
        GetAclInformation(
            old,
            (&raw mut size).cast(),
            size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )
    };
    if sized == 0 {
        return Err(std::io::Error::last_os_error());
    }
    // u32s, so the list is aligned; the old one's size always suffices.
    let mut new = vec![0u32; (size.AclBytesInUse as usize).div_ceil(4)];
    // SAFETY: the caller promises a live list.
    let revision = unsafe { (*old).AclRevision } as u32;
    // SAFETY: `new` is writable for the length given.
    if unsafe { InitializeAcl(new.as_mut_ptr().cast(), (new.len() * 4) as u32, revision) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut removed = false;
    for index in 0..size.AceCount {
        let mut ace = null_mut();
        // SAFETY: `index` is below the list's count.
        if unsafe { GetAce(old, index, &mut ace) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: every entry starts with a header.
        let header = unsafe { *(ace as *const ACE_HEADER) };
        let simple = matches!(
            header.AceType as u32,
            ACCESS_ALLOWED_ACE_TYPE | ACCESS_DENIED_ACE_TYPE
        );
        // SAFETY: an allowed or denied entry ends with its SID, at the same
        // place in both.
        let own = simple
            && header.AceFlags as u32 & INHERITED_ACE == 0
            && unsafe {
                EqualSid(
                    (&raw mut (*(ace as *mut ACCESS_ALLOWED_ACE)).SidStart).cast(),
                    sid,
                )
            } != 0;
        if own {
            removed = true;
            continue;
        }
        // SAFETY: `ace` is `header.AceSize` bytes, and `new` has the room.
        if unsafe {
            AddAce(
                new.as_mut_ptr().cast(),
                revision,
                MAXDWORD,
                ace,
                header.AceSize as u32,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(removed.then_some(new))
}

/// Adds to the access list of `grant`'s path an entry for `sid`, inherited
/// by everything below a folder (but a folder's to enter).
fn set_entry(grant: &Grant, sid: PSID) -> std::io::Result<()> {
    let (access, mode) = match grant.access {
        Access::Read => (READ, GRANT_ACCESS),
        Access::Write => (WRITE, GRANT_ACCESS),
        Access::Deny => (WRITE, DENY_ACCESS),
        Access::Enter => (ENTER, GRANT_ACCESS),
    };
    let inherited = grant.access != Access::Enter;
    with_dacl(&grant.path, inherited, |old, folder, write| {
        let entry = EXPLICIT_ACCESS_W {
            grfAccessPermissions: access,
            grfAccessMode: mode,
            grfInheritance: if inherited && folder {
                SUB_CONTAINERS_AND_OBJECTS_INHERIT
            } else {
                NO_INHERITANCE
            },
            Trustee: TRUSTEE_W {
                pMultipleTrustee: null_mut(),
                MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_UNKNOWN,
                ptstrName: sid.cast(),
            },
        };
        let mut new: *mut ACL = null_mut();
        // SAFETY: one entry in, `old` live meanwhile, `new` freed below.
        let merged = unsafe { SetEntriesInAclW(1, &entry, old, &mut new) };
        if merged != ERROR_SUCCESS {
            return Err(std::io::Error::from_raw_os_error(merged as i32));
        }
        let written = write(new);
        // SAFETY: allocated by SetEntriesInAclW, freed once.
        unsafe { LocalFree(new.cast()) };
        written
    })
}

/// Runs this worker's own command line again, in the container `sid` and a
/// Job object that dies with this launcher; its exit code.
fn run_inside(sid: &Sid, policy: &Policy) -> Result<u32, String> {
    let failed = |what: &str| format!("{what}: {}", std::io::Error::last_os_error());
    let internet = policy
        .network
        .then(|| Sid::from_string(INTERNET_CLIENT))
        .transpose()?;
    let mut capabilities: Vec<SID_AND_ATTRIBUTES> = internet
        .iter()
        .map(|sid| SID_AND_ATTRIBUTES {
            Sid: sid.0,
            Attributes: SE_GROUP_ENABLED as u32,
        })
        .collect();
    let mut security = SECURITY_CAPABILITIES {
        AppContainerSid: sid.0,
        Capabilities: if capabilities.is_empty() {
            null_mut()
        } else {
            capabilities.as_mut_ptr()
        },
        CapabilityCount: capabilities.len() as u32,
        Reserved: 0,
    };
    // The worker's stdio is this launcher's, handed on, and nothing else.
    // SAFETY: GetStdHandle takes constants and returns handles or null.
    let mut stdio = unsafe {
        [
            GetStdHandle(STD_INPUT_HANDLE),
            GetStdHandle(STD_OUTPUT_HANDLE),
            GetStdHandle(STD_ERROR_HANDLE),
        ]
    };
    for handle in stdio {
        // SAFETY: marks this process's own stdio handle inheritable.
        unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) };
    }
    let attributes = Attributes::new(2)?;
    attributes.set(
        PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
        (&mut security as *mut SECURITY_CAPABILITIES).cast(),
        size_of::<SECURITY_CAPABILITIES>(),
    )?;
    attributes.set(
        PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
        stdio.as_mut_ptr().cast(),
        size_of_val(&stdio),
    )?;

    // SAFETY: a zeroed STARTUPINFOEXW is valid once its size is set.
    let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    // Outside the interactive window station, a desk of this call's own.
    let desk = Desk::for_container(sid)?;
    if let Some(desk) = &desk {
        startup.StartupInfo.lpDesktop = desk.name.as_ptr().cast_mut();
    }
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = stdio[0];
    startup.StartupInfo.hStdOutput = stdio[1];
    startup.StartupInfo.hStdError = stdio[2];
    startup.lpAttributeList = attributes.list;

    let job = Job::new()?;
    // SAFETY: setting the variable before any other thread starts, as
    // `confine`'s callers promise; the worker inherits it.
    unsafe { std::env::set_var(INNER, "1") };
    let program: Vec<u16> = wide(
        std::env::current_exe()
            .map_err(|err| err.to_string())?
            .as_os_str(),
    );
    // SAFETY: CreateProcessW may write to the command line it is given, so
    // it gets a copy of this process's own.
    let mut command = unsafe { copy_wide(GetCommandLineW()) };
    // SAFETY: zeroed is a valid PROCESS_INFORMATION to receive into.
    let mut process: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: every pointer is live for the call; the worker starts
    // suspended so it is in the job before it runs.
    let started = unsafe {
        CreateProcessW(
            program.as_ptr(),
            command.as_mut_ptr(),
            null(),
            null(),
            1,
            CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT,
            null(),
            null(),
            &startup.StartupInfo,
            &mut process,
        )
    };
    if started == 0 {
        return Err(failed("cannot start the worker in its container"));
    }
    // SAFETY: the worker's handles, live until closed below.
    let assigned = unsafe { AssignProcessToJobObject(job.0, process.hProcess) };
    if assigned == 0 {
        // SAFETY: ends the suspended worker that could not join the job.
        unsafe { TerminateProcess(process.hProcess, 1) };
        let message = failed("cannot put the worker in its job");
        unsafe {
            CloseHandle(process.hThread);
            CloseHandle(process.hProcess);
        }
        return Err(message);
    }
    // SAFETY: the worker's main thread, suspended above.
    unsafe {
        ResumeThread(process.hThread);
        CloseHandle(process.hThread);
    }
    let limit = policy.kill_after.map_or(INFINITE, |seconds| {
        (seconds * 1000).min(u32::MAX as u64 - 1) as u32
    });
    // SAFETY: the worker's process handle, live until closed below.
    if unsafe { WaitForSingleObject(process.hProcess, limit) } == WAIT_TIMEOUT {
        // SAFETY: ends the worker and everything it started.
        unsafe {
            TerminateJobObject(job.0, 1);
            WaitForSingleObject(process.hProcess, INFINITE);
        }
    }
    let mut code = 1u32;
    // SAFETY: the worker has ended; `code` is writable for its exit code.
    unsafe {
        GetExitCodeProcess(process.hProcess, &mut code);
        CloseHandle(process.hProcess);
    }
    Ok(code)
}

/// A window station and a desktop of a worker's own, for a launcher outside
/// the interactive window station (a host started over ssh, or as a
/// service). Windows starts no program without a window station and desktop
/// it may open, and an AppContainer may open the interactive ones, whose
/// access lists admit every app container, but not a service's: user32
/// fails as it starts (0xC0000142). These admit the user and this call's
/// container alone (an AppContainer needs both), and go with their handles
/// when the worker has ended.
struct Desk {
    station: HWINSTA,
    desktop: HDESK,
    /// `station\desktop`, NUL-terminated, as `lpDesktop` names it.
    name: Vec<u16>,
}

impl Desk {
    /// None on the interactive window station, which the worker may open.
    fn for_container(sid: &Sid) -> Result<Option<Desk>, String> {
        let failed = |what: &str| format!("{what}: {}", std::io::Error::last_os_error());
        // SAFETY: this process's window station; not ours to close.
        let current = unsafe { GetProcessWindowStation() };
        let mut flags = USEROBJECTFLAGS::default();
        // SAFETY: `flags` is writable for its size.
        let read = unsafe {
            GetUserObjectInformationW(
                current,
                UOI_FLAGS,
                (&mut flags as *mut USEROBJECTFLAGS).cast(),
                size_of::<USEROBJECTFLAGS>() as u32,
                null_mut(),
            )
        };
        // `WSF_VISIBLE`: the interactive window station.
        if read != 0 && flags.dwFlags & 1 != 0 {
            return Ok(None);
        }
        let access = format!("D:P(A;;GA;;;{})(A;;GA;;;{})", sid.text()?, user_sid()?);
        let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
        // SAFETY: a NUL-terminated SDDL string; the descriptor is ours to free.
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide(OsStr::new(&access)).as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                null_mut(),
            )
        } == 0
        {
            return Err(failed("cannot describe the worker's window station"));
        }
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        let station_name = format!("latch.worker.{:016x}", rand::random::<u64>());
        // `CWF_CREATE_ONLY`; every window-station-specific right (0x37F).
        // SAFETY: a NUL-terminated name; `attributes` is live for the call.
        let station = unsafe {
            CreateWindowStationW(
                wide(OsStr::new(&station_name)).as_ptr(),
                1,
                0x37F,
                &attributes,
            )
        };
        let made = if station.is_null() {
            Err(failed("cannot make the worker's window station"))
        } else {
            // The desktop is made in the process's window station: this
            // one, for the moment.
            // SAFETY: our new window station, then this process's own back.
            unsafe {
                let desktop = match SetProcessWindowStation(station) {
                    0 => null_mut(),
                    // Every desktop-specific right (0x1FF).
                    _ => CreateDesktopW(
                        wide(OsStr::new("worker")).as_ptr(),
                        null(),
                        null(),
                        0,
                        0x1FF,
                        &attributes,
                    ),
                };
                // Read before switching back, which may set another error.
                let failure = failed("cannot make the worker's desktop");
                SetProcessWindowStation(current);
                match (!desktop.is_null()).then_some(desktop) {
                    None => {
                        CloseWindowStation(station);
                        Err(failure)
                    }
                    Some(desktop) => Ok(Desk {
                        station,
                        desktop,
                        name: wide(OsStr::new(&format!("{station_name}\\worker"))),
                    }),
                }
            }
        };
        // SAFETY: the descriptor converted above, freed once.
        unsafe { LocalFree(descriptor) };
        made.map(Some)
    }
}

impl Drop for Desk {
    fn drop(&mut self) {
        // SAFETY: our own handles, closed once.
        unsafe {
            CloseDesktop(self.desktop);
            CloseWindowStation(self.station);
        }
    }
}

/// This process's user, as an SDDL SID string.
fn user_sid() -> Result<String, String> {
    let mut token: HANDLE = null_mut();
    // SAFETY: a query-only token of this process, closed below.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(format!(
            "cannot read this user: {}",
            std::io::Error::last_os_error()
        ));
    }
    let mut buffer = vec![0u8; 256];
    let mut size = 0u32;
    // SAFETY: `buffer` is writable for its length; the token is ours.
    let read = unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            buffer.len() as u32,
            &mut size,
        )
    };
    // SAFETY: the token is ours and closed once.
    unsafe { CloseHandle(token) };
    if read == 0 {
        return Err(format!(
            "cannot read this user: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: GetTokenInformation filled a TOKEN_USER at the buffer's start.
    let user = unsafe { (*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    sid_text(user)
}

/// `sid` as an SDDL SID string.
fn sid_text(sid: PSID) -> Result<String, String> {
    let mut text: *mut u16 = null_mut();
    // SAFETY: a valid SID; `text` is LocalAlloc'd by Windows, freed below.
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        return Err(format!(
            "cannot name a SID: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: a NUL-terminated string Windows made.
    let length = (0..)
        .take_while(|&at| unsafe { *text.add(at) } != 0)
        .count();
    // SAFETY: `length` u16s at `text`, read before it is freed.
    let named = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, length) });
    // SAFETY: Windows' allocation, freed once.
    unsafe { LocalFree(text.cast()) };
    Ok(named)
}

/// A SID Windows allocated, freed when dropped.
struct Sid(PSID, fn(PSID));

impl Sid {
    /// As an SDDL SID string.
    fn text(&self) -> Result<String, String> {
        sid_text(self.0)
    }

    /// The SID of container profile `name`, made now.
    fn container(name: &str) -> Result<Sid, String> {
        let name = wide(OsStr::new(name));
        let mut sid: PSID = null_mut();
        // SAFETY: NUL-terminated names, no capabilities, `sid` written on success.
        let made = unsafe {
            CreateAppContainerProfile(
                name.as_ptr(),
                name.as_ptr(),
                name.as_ptr(),
                null(),
                0,
                &mut sid,
            )
        };
        if made < 0 {
            // SAFETY: the same name, made by an earlier attempt of ours.
            let derived =
                unsafe { DeriveAppContainerSidFromAppContainerName(name.as_ptr(), &mut sid) };
            if derived < 0 {
                return Err(format!(
                    "cannot make the worker's container: HRESULT {made:#x}"
                ));
            }
        }
        // SAFETY: these SIDs are freed with FreeSid.
        Ok(Sid(sid, |sid| unsafe {
            FreeSid(sid);
        }))
    }

    fn from_string(text: &str) -> Result<Sid, String> {
        let text = wide(OsStr::new(text));
        let mut sid: PSID = null_mut();
        // SAFETY: a NUL-terminated SID string; `sid` written on success.
        if unsafe { ConvertStringSidToSidW(text.as_ptr(), &mut sid) } == 0 {
            return Err(format!(
                "cannot read a SID: {}",
                std::io::Error::last_os_error()
            ));
        }
        // SAFETY: these SIDs are freed with LocalFree.
        Ok(Sid(sid, |sid| unsafe {
            LocalFree(sid);
        }))
    }
}

impl Drop for Sid {
    fn drop(&mut self) {
        (self.1)(self.0);
    }
}

/// A process attribute list, deleted and freed when dropped.
struct Attributes {
    list: LPPROC_THREAD_ATTRIBUTE_LIST,
    /// The room the list lives in.
    _room: Vec<u8>,
}

impl Attributes {
    fn new(count: u32) -> Result<Attributes, String> {
        let mut size = 0usize;
        // SAFETY: the first call only says how much room the list needs.
        unsafe { InitializeProcThreadAttributeList(null_mut(), count, 0, &mut size) };
        let mut room = vec![0u8; size];
        let list: LPPROC_THREAD_ATTRIBUTE_LIST = room.as_mut_ptr().cast();
        // SAFETY: `room` is `size` bytes, as asked for.
        if unsafe { InitializeProcThreadAttributeList(list, count, 0, &mut size) } == 0 {
            return Err(format!("attributes: {}", std::io::Error::last_os_error()));
        }
        Ok(Attributes { list, _room: room })
    }

    /// Sets attribute `which` to the `size` bytes at `value`, which must
    /// outlive the list.
    fn set(&self, which: usize, value: *mut std::ffi::c_void, size: usize) -> Result<(), String> {
        // SAFETY: the caller keeps `value` alive for the list's life.
        let ok = unsafe {
            UpdateProcThreadAttribute(self.list, 0, which, value, size, null_mut(), null())
        };
        if ok == 0 {
            return Err(format!("attribute: {}", std::io::Error::last_os_error()));
        }
        Ok(())
    }
}

impl Drop for Attributes {
    fn drop(&mut self) {
        // SAFETY: initialized in `new`, deleted once; `_room` frees after.
        unsafe { DeleteProcThreadAttributeList(self.list) };
    }
}

/// A Job object that ends everything in it when its last handle closes.
struct Job(HANDLE);

impl Job {
    fn new() -> Result<Job, String> {
        // SAFETY: an unnamed job with default security.
        let job = unsafe { CreateJobObjectW(null(), null()) };
        if job.is_null() {
            return Err(format!(
                "cannot make the worker's job: {}",
                std::io::Error::last_os_error()
            ));
        }
        // SAFETY: a zeroed limit structure is valid; only the flags are set.
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags =
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
        // SAFETY: `limits` is the structure this class takes, for its size.
        let set = unsafe {
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        let job = Job(job);
        if set == 0 {
            return Err(format!(
                "cannot set the worker's job: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(job)
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: the job's one handle; closing it ends what is still in it.
        unsafe { CloseHandle(self.0) };
    }
}

/// Puts the launcher `process` in the host's own job, so that it, and the
/// worker it starts, end when the host does, however the host ends.
pub fn tie_to_host(process: HANDLE) {
    static HOST_JOB: OnceLock<Option<usize>> = OnceLock::new();
    let job = HOST_JOB.get_or_init(|| {
        Job::new().ok().map(|job| {
            let handle = job.0 as usize;
            // Kept for the host's whole life.
            std::mem::forget(job);
            handle
        })
    });
    if let Some(job) = job {
        // SAFETY: the host's job and a live launcher of its own.
        unsafe { AssignProcessToJobObject(*job as HANDLE, process) };
    }
}

/// Takes back the grants of launchers that were killed before they could:
/// every ledger of the host on `state_dir` whose launcher no longer runs.
pub fn sweep(state_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(ledgers(state_dir)) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with("latch.worker.") || !name.ends_with(".json") {
            continue;
        }
        let Some(ledger) = std::fs::read(entry.path())
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Ledger>(&bytes).ok())
        else {
            continue;
        };
        if running(ledger.launcher) {
            continue;
        }
        if let Ok(sid) = Sid::container(&ledger.container) {
            take_back(&sid, &ledger.grants);
        }
        // SAFETY: a NUL-terminated profile name from our own ledger.
        unsafe { DeleteAppContainerProfile(wide(OsStr::new(&ledger.container)).as_ptr()) };
        let _ = std::fs::remove_file(entry.path());
    }
}

/// Whether process `pid` runs.
fn running(pid: u32) -> bool {
    // SAFETY: OpenProcess takes plain values; null is failure.
    let process = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    if process.is_null() {
        return false;
    }
    // SAFETY: a live handle of ours, closed once.
    let alive = unsafe { WaitForSingleObject(process, 0) } != WAIT_OBJECT_0;
    unsafe { CloseHandle(process) };
    alive
}

fn wide(text: &OsStr) -> Vec<u16> {
    text.encode_wide().chain([0]).collect()
}

/// A copy of the NUL-terminated wide string at `text`, NUL included.
///
/// # Safety
/// `text` must point to a NUL-terminated wide string.
unsafe fn copy_wide(text: *const u16) -> Vec<u16> {
    let mut length = 0;
    // SAFETY: the caller promises a terminator.
    while unsafe { *text.add(length) } != 0 {
        length += 1;
    }
    // SAFETY: `length` units then the terminator are readable.
    unsafe { std::slice::from_raw_parts(text, length + 1) }.to_vec()
}

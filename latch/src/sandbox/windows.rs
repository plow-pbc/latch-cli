//! The Windows worker sandbox: an AppContainer inside a Job object.
//!
//! Windows cannot confine a running process, so `confine` works as Linux's
//! does: the first copy of the worker is a launcher, outside any sandbox, and
//! starts the same worker again inside one, then waits for it and ends as it
//! ended. The launcher makes an AppContainer profile of the call's own, so no
//! two calls share what they were granted, and gives that container exactly
//! the policy's paths: its areas and temp folder to read and write, its reads
//! in home (and the latch it runs) to read. Outside home it reads what any
//! AppContainer may, the system's own folders. A container reaches nothing
//! else of the user's, and no network unless the policy grants it; loopback
//! is closed to it, so the host's local socket is too.
//!
//! A deny entry doesn't bind a container: what an entry for it, for one of
//! its capabilities or for every container allows, it may do, even with an
//! entry denying its own SID ahead of that one. A path stays out of
//! a container's reach only while no grant reaches it. So a hidden path
//! inside a grant must have a protected access list that allows no container
//! (latch makes its state folders so); one that hasn't fails the call
//! (`sealed`). The judge keeps every write grant off the folder of the
//! latch the host runs, and every grant off AppData (`never_readable`).
//!
//! Unlike macOS and Linux, an `exec` worker does not read all of home here:
//! that would mean an entry on every file in the profile. It reads its areas
//! and what the policy names; home-wide reading is left for an opt-in.
//!
//! The worker runs in a Job object that kills everything in it when its last
//! handle closes: the launcher holds it, so a launcher that ends, however it
//! ends, takes the worker and every program it started with it. `kill_after`
//! is the launcher's: it ends the job then. The host keeps every launcher in a
//! job of its own (`tie_to_host`), so a host that dies takes them all: a
//! launcher starts suspended, and one that cannot join that job never runs.
//!
//! The grants are written into the files' own access lists, so the launcher
//! takes them back when the worker ends. It first writes what it grants to a
//! ledger in the state folder (not the temp folder, which for a launcher is
//! the call's own and goes with it); a launcher that was killed leaves its
//! ledger, and the host takes those grants back (`sweep`): when it kills a
//! launcher, and when it starts.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_SUCCESS, HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, LocalFree,
    SetHandleInformation, WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Authorization::{
    ACCESS_MODE, ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
    ConvertStringSidToSidW, EXPLICIT_ACCESS_W, GRANT_ACCESS, GetSecurityInfo, NO_MULTIPLE_TRUSTEE,
    SDDL_REVISION_1, SE_FILE_OBJECT, SetEntriesInAclW, SetNamedSecurityInfoW, TRUSTEE_IS_SID,
    TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
};
use windows_sys::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeleteAppContainerProfile, DeriveAppContainerSidFromAppContainerName,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_FLAGS, ACE_HEADER, ACL, ACL_SIZE_INFORMATION, AclSizeInformation,
    AddAce, DACL_SECURITY_INFORMATION, EqualSid, FreeSid, GetAce, GetAclInformation,
    GetSecurityDescriptorControl, GetSidIdentifierAuthority, GetTokenInformation, INHERIT_ONLY_ACE,
    INHERITED_ACE, InitializeAcl, InitializeSecurityDescriptor, NO_INHERITANCE,
    PSECURITY_DESCRIPTOR, PSID, SE_DACL_AUTO_INHERITED, SE_DACL_PROTECTED, SECURITY_ATTRIBUTES,
    SECURITY_CAPABILITIES, SECURITY_DESCRIPTOR, SID_AND_ATTRIBUTES,
    SUB_CONTAINERS_AND_OBJECTS_INHERIT, SetKernelObjectSecurity, SetSecurityDescriptorControl,
    SetSecurityDescriptorDacl, TOKEN_QUERY, TOKEN_USER, TokenIsAppContainer, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    DELETE, FILE_EXECUTE, FILE_FLAG_BACKUP_SEMANTICS, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ,
    FILE_GENERIC_WRITE, FILE_READ_ATTRIBUTES, FILE_READ_DATA, FILE_SHARE_READ, FILE_SHARE_WRITE,
    FILE_TRAVERSE, GetDriveTypeW, GetFinalPathNameByHandleW, GetLogicalDriveStringsW, READ_CONTROL,
    SYNCHRONIZE, WRITE_DAC,
};
use windows_sys::Win32::System::Console::{
    GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
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
    OpenProcess, OpenProcessToken, OpenThread, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, PROCESS_INFORMATION, PROCESS_SYNCHRONIZE,
    ReleaseMutex, ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOEXW, THREAD_SUSPEND_RESUME,
    TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
};
use windows_sys::Win32::System::WindowsProgramming::DRIVE_FIXED;

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

/// latch's own capability, which every worker carries: as
/// `DeriveCapabilitySidsFromName` derives it from `latchWorkerVolumeRoot`.
/// Programs open a volume's root for its details (cmd's `dir` and `vol`),
/// and a root admits no AppContainer, so they fail even in a folder the
/// worker may list. `latch setup`, through Windows' administrator prompt
/// once, gives this capability `ROOT_ACCESS` on each fixed volume's root,
/// and nothing below it. Windows' own `isolatedWin32-volumeRootMinimal`, which roots already
/// admit, cannot be given to a plain AppContainer: CreateProcess refuses it.
pub(crate) const VOLUME_ROOT: &str = "S-1-15-3-1024-2072464065-3400578307-2891438376-2029114129-1094856869-133864159-224730807-2445515868";

/// What a root grants it: the root's own attributes, which the volume's
/// details are read through, and nothing to list or read.
const ROOT_ACCESS: u32 = FILE_READ_ATTRIBUTES | SYNCHRONIZE;

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
            .any(|home| within(path, home))
    };
    let reads = policy.reads.iter().filter(|read| in_home(read));
    // The latch it runs, wherever it is, unless every container may run it
    // already (Program Files), whose access list this user may not change;
    // and the helper the host checked (gog), wherever it is: a state folder
    // outside home keeps its gog there.
    let program_open = every_container_runs(&program);
    let program_read = std::iter::once(&program).filter(|_| !program_open);
    for read in reads.chain(&policy.program).chain(program_read) {
        if read.exists() {
            grants.push(Grant {
                path: read.clone(),
                access: Access::Read,
            });
        }
    }
    // No write grant holds a hidden path, the state folder or the folder of
    // the latch it runs. A sealed folder keeps what is in it out of reach,
    // not its name: a worker that may delete a folder above it could rename
    // that folder and make another in its place, which the host would then
    // read as its own (its rules, say). The judge refuses such paths; this
    // fails closed should one get here.
    let hidden = policy
        .hidden
        .iter()
        .chain(std::iter::once(&policy.state_dir));
    let kept: Vec<&Path> = hidden
        .clone()
        .map(PathBuf::as_path)
        .chain(program.parent())
        .collect();
    for area in &policy.areas {
        if let Some(path) = kept.iter().find(|path| within(path, area)) {
            return Err(format!(
                "{} would be written, and holds {}, which no worker may move",
                area.display(),
                path.display()
            ));
        }
    }
    // A hidden path inside a folder it reads stays hidden only if that grant
    // never reaches it.
    let granted: Vec<PathBuf> = grants.iter().map(|grant| grant.path.clone()).collect();
    for path in hidden {
        if path.exists()
            && granted.iter().any(|open| within(path, open))
            && !sealed(path).map_err(|err| format!("{}: {err}", path.display()))?
        {
            return Err(format!(
                "{} is hidden but inside a folder the call is granted, and its access list does not keep that grant out",
                path.display()
            ));
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

/// Access-list edits, one at a time across this user's launchers, and a
/// put's replacing a file with that file's list (`files/place.rs`). Each is
/// a read, a change and a write of the whole list, so two at once write
/// over each other: a grant lost, or an entry taken back that comes back.
/// The latch it runs and any area two calls share are edited by both. A
/// launcher killed while holding it abandons it, which frees it; a
/// container cannot reach it (its `Local` names are its own).
pub(crate) struct EditLock(HANDLE);

impl EditLock {
    pub(crate) fn take() -> std::io::Result<EditLock> {
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

/// Takes back every entry for the container.
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

/// Whether `path` is `folder` or inside it, as Windows matches names: with
/// or without a `\\?\` prefix, in any case.
pub(crate) fn within(path: &Path, folder: &Path) -> bool {
    let names = |path: &Path| -> Vec<String> {
        let text = path.to_string_lossy();
        let plain = Path::new(text.strip_prefix(r"\\?\").unwrap_or(&text)).to_path_buf();
        plain
            .components()
            .map(|name| name.as_os_str().to_string_lossy().to_lowercase())
            .collect()
    };
    names(path).starts_with(&names(folder))
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

/// What the SIDs of containers, their capabilities and every container
/// (`S-1-15-…`) are issued by.
const APP_PACKAGE_AUTHORITY: [u8; 6] = [0, 0, 0, 0, 0, 15];

/// Whether no grant above `path` reaches it: its access list is protected
/// (it inherits nothing) and allows no container, capability or every
/// container. The one way Windows keeps a path inside a grant from a
/// container: a deny entry doesn't bind one.
fn sealed(path: &Path) -> std::io::Result<bool> {
    // SAFETY: `read_list` hands over a live descriptor and its list.
    read_list(path, |descriptor, list| unsafe {
        sealed_list(descriptor, list)
    })
}

/// Whether every container may read and run the file at `path` already.
fn every_container_runs(path: &Path) -> bool {
    let needed = FILE_READ_DATA | FILE_EXECUTE;
    let every = Sid::from_string("S-1-15-2-1");
    let look = |_, list: *const ACL| -> std::io::Result<bool> {
        let Ok(every) = &every else { return Ok(false) };
        // SAFETY: `read_list` hands over a live list.
        for ace in unsafe { entries(list) }? {
            // SAFETY: every entry starts with a header.
            let header = unsafe { *(ace as *const ACE_HEADER) };
            if header.AceType as u32 != ACCESS_ALLOWED_ACE_TYPE
                || header.AceFlags as u32 & INHERIT_ONLY_ACE != 0
            {
                continue;
            }
            let allowed = ace as *mut ACCESS_ALLOWED_ACE;
            // SAFETY: an allowed entry, its mask then its SID.
            let (mask, theirs) = unsafe {
                (
                    (*allowed).Mask,
                    EqualSid((&raw mut (*allowed).SidStart).cast(), every.0) != 0,
                )
            };
            if theirs && mask & needed == needed {
                return Ok(true);
            }
        }
        Ok(false)
    };
    read_list(path, look).unwrap_or(false)
}

/// What `look` makes of the access list of `path`, given with the
/// descriptor that holds it.
fn read_list<T>(
    path: &Path,
    look: impl FnOnce(PSECURITY_DESCRIPTOR, *const ACL) -> std::io::Result<T>,
) -> std::io::Result<T> {
    use std::os::windows::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .access_mode(READ_CONTROL)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;
    let mut list: *mut ACL = null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: a live handle with READ_CONTROL; the descriptor holding `list`
    // is freed below with LocalFree.
    let read = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            &mut list,
            null_mut(),
            &mut descriptor,
        )
    };
    if read != ERROR_SUCCESS {
        return Err(std::io::Error::from_raw_os_error(read as i32));
    }
    // Both stay live until the descriptor is freed, below.
    let answer = look(descriptor, list);
    // SAFETY: allocated by GetSecurityInfo, freed once.
    unsafe { LocalFree(descriptor) };
    answer
}

/// The entries of the access list `list`.
///
/// # Safety
/// `list` must be a live access list.
unsafe fn entries(list: *const ACL) -> std::io::Result<Vec<*mut std::ffi::c_void>> {
    let mut size = ACL_SIZE_INFORMATION::default();
    // SAFETY: `size` is writable for its own length.
    let sized = unsafe {
        GetAclInformation(
            list,
            (&raw mut size).cast(),
            size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )
    };
    if sized == 0 {
        return Err(std::io::Error::last_os_error());
    }
    (0..size.AceCount)
        .map(|index| {
            let mut ace = null_mut();
            // SAFETY: `index` is below the list's count.
            match unsafe { GetAce(list, index, &mut ace) } {
                0 => Err(std::io::Error::last_os_error()),
                _ => Ok(ace),
            }
        })
        .collect()
}

/// `sealed`, for a descriptor and its access list.
///
/// # Safety
/// `descriptor` must be live, and `list` null or its live access list.
unsafe fn sealed_list(descriptor: PSECURITY_DESCRIPTOR, list: *const ACL) -> std::io::Result<bool> {
    let (mut control, mut revision) = (0u16, 0u32);
    // SAFETY: as the caller promised.
    if unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    // A null list allows everyone everything.
    if control & SE_DACL_PROTECTED == 0 || list.is_null() {
        return Ok(false);
    }
    // SAFETY: as the caller promised.
    for ace in unsafe { entries(list) }? {
        // SAFETY: every entry starts with a header.
        let header = unsafe { *(ace as *const ACE_HEADER) };
        match header.AceType as u32 {
            ACCESS_DENIED_ACE_TYPE => {}
            ACCESS_ALLOWED_ACE_TYPE => {
                // SAFETY: an allowed entry ends with its SID, which is valid.
                let authority = unsafe {
                    *GetSidIdentifierAuthority(
                        (&raw mut (*(ace as *mut ACCESS_ALLOWED_ACE)).SidStart).cast(),
                    )
                };
                if authority.Value == APP_PACKAGE_AUTHORITY {
                    return Ok(false);
                }
            }
            // An entry of another kind may allow anyone: no proof.
            _ => return Ok(false),
        }
    }
    Ok(true)
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
    // SAFETY: a plain C struct, valid as zeroes; it is initialized below.
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
        Access::Enter => (ENTER, GRANT_ACCESS),
    };
    let inherited = grant.access != Access::Enter;
    with_dacl(&grant.path, inherited, |old, folder, write| {
        let inheritance = if inherited && folder {
            SUB_CONTAINERS_AND_OBJECTS_INHERIT
        } else {
            NO_INHERITANCE
        };
        // SAFETY: `old` is the object's access list, live meanwhile.
        unsafe { add_entry(old, access, mode, inheritance, sid, write) }
    })
}

/// Writes `old` with one more entry for `sid`.
///
/// # Safety
/// `old` must be null or a live access list, `sid` a live SID.
unsafe fn add_entry(
    old: *mut ACL,
    access: u32,
    mode: ACCESS_MODE,
    inheritance: ACE_FLAGS,
    sid: PSID,
    write: &dyn Fn(*mut ACL) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let entry = EXPLICIT_ACCESS_W {
        grfAccessPermissions: access,
        grfAccessMode: mode,
        grfInheritance: inheritance,
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
}

/// The fixed volumes' roots (`C:\\` and the like), where a worker's
/// folders are.
fn fixed_roots() -> Vec<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    let mut buffer = vec![0u16; 512];
    // SAFETY: `buffer` is writable for its length.
    let length =
        unsafe { GetLogicalDriveStringsW(buffer.len() as u32, buffer.as_mut_ptr()) } as usize;
    if length == 0 || length > buffer.len() {
        return Vec::new();
    }
    buffer[..length]
        .split(|&unit| unit == 0)
        .filter(|root| !root.is_empty())
        .map(|root| PathBuf::from(std::ffi::OsString::from_wide(root)))
        // SAFETY: a NUL-terminated name.
        .filter(|root| unsafe { GetDriveTypeW(wide(root.as_os_str()).as_ptr()) } == DRIVE_FIXED)
        .collect()
}

/// Whether `root` admits the workers' capability as `grant_listing` makes
/// it: an entry of its own allowing at least `ROOT_ACCESS`. A root with no
/// access list (FAT) admits every program already. Reading the list needs
/// no administrator.
fn admits(root: &Path, sid: &Sid) -> std::io::Result<bool> {
    read_list(root, |_, list| {
        // SAFETY: `list` is null or the live list the descriptor holds.
        Ok(list.is_null() || unsafe { allowed(list, sid.0) }? & ROOT_ACCESS == ROOT_ACCESS)
    })
}

/// What `list`'s own (not inherited) allowing entries give `sid`.
///
/// # Safety
/// `list` must be a live access list.
unsafe fn allowed(list: *const ACL, sid: PSID) -> std::io::Result<u32> {
    let mut access = 0;
    // SAFETY: the caller's live list.
    for ace in unsafe { entries(list) }? {
        // SAFETY: every entry starts with a header; an allowed entry goes on
        // with its mask and its SID.
        unsafe {
            let header = *(ace as *const ACE_HEADER);
            let allowing = ace as *mut ACCESS_ALLOWED_ACE;
            if header.AceType as u32 == ACCESS_ALLOWED_ACE_TYPE
                && header.AceFlags as u32 & INHERITED_ACE == 0
                && EqualSid((&raw mut (*allowing).SidStart).cast(), sid) != 0
            {
                access |= (*allowing).Mask;
            }
        }
    }
    Ok(access)
}

/// Whether agents may list folders here: `granted` when every fixed
/// volume's root admits the workers' capability, `missing` when none does
/// (or none can be read), `partial` in between.
pub fn list_folders() -> Option<&'static str> {
    let Ok(sid) = Sid::from_string(VOLUME_ROOT) else {
        return Some("missing");
    };
    let roots = fixed_roots();
    let admitting = roots
        .iter()
        .filter(|root| admits(root, &sid).unwrap_or(false))
        .count();
    Some(match admitting {
        0 => "missing",
        n if n == roots.len() => "granted",
        _ => "partial",
    })
}

/// Whether agents list folders on the volume `path` is on: whether its
/// root admits the workers' capability.
#[cfg(test)]
pub(crate) fn lists_in(path: &Path) -> bool {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::Storage::FileSystem::GetVolumePathNameW;
    let mut root = vec![0u16; 1024];
    // SAFETY: a NUL-terminated path; `root` is writable for its length.
    let found = unsafe {
        GetVolumePathNameW(
            wide(path.as_os_str()).as_ptr(),
            root.as_mut_ptr(),
            root.len() as u32,
        )
    };
    let length = root.iter().position(|&unit| unit == 0).unwrap_or(0);
    let root = PathBuf::from(std::ffi::OsString::from_wide(&root[..length]));
    found != 0
        && Sid::from_string(VOLUME_ROOT).is_ok_and(|sid| admits(&root, &sid).unwrap_or(false))
}

/// `latch grant-listing`, which `latch setup` runs as administrator
/// (writing a root's access list needs one): gives the workers' capability
/// `ROOT_ACCESS` on each fixed volume's root that lacks it, its own entry
/// with nothing inherited below. Each root it could not change, and why.
pub fn grant_listing() -> Vec<String> {
    let sid = match Sid::from_string(VOLUME_ROOT) {
        Ok(sid) => sid,
        Err(message) => return vec![message],
    };
    let roots = fixed_roots();
    if roots.is_empty() {
        return vec!["no fixed volume found".into()];
    }
    roots
        .into_iter()
        .filter_map(|root| {
            let granted = grant_root(&root, &sid);
            granted
                .err()
                .map(|why| format!("{}: {why}", root.display()))
        })
        .collect()
}

/// Gives `sid` `ROOT_ACCESS` on `root` unless it has it: an entry of its
/// own, which nothing below inherits.
fn grant_root(root: &Path, sid: &Sid) -> Result<(), String> {
    if admits(root, sid).unwrap_or(false) {
        return Ok(());
    }
    let added = with_dacl(root, false, |old, _, write| {
        // SAFETY: `old` is the root's access list, live meanwhile; `sid` is
        // live.
        unsafe { add_entry(old, ROOT_ACCESS, GRANT_ACCESS, NO_INHERITANCE, sid.0, write) }
    });
    match (added, admits(root, sid)) {
        (Err(err), _) | (_, Err(err)) => Err(err.to_string()),
        (Ok(()), Ok(true)) => Ok(()),
        (Ok(()), Ok(false)) => Err("its entry did not change".into()),
    }
}

/// Runs this worker's own command line again, in the container `sid` and a
/// Job object that dies with this launcher; its exit code.
fn run_inside(sid: &Sid, policy: &Policy) -> Result<u32, String> {
    let failed = |what: &str| format!("{what}: {}", std::io::Error::last_os_error());
    let mut granted = vec![Sid::from_string(VOLUME_ROOT)?];
    if policy.network {
        granted.push(Sid::from_string(INTERNET_CLIENT)?);
    }
    let mut capabilities: Vec<SID_AND_ATTRIBUTES> = granted
        .iter()
        .map(|sid| SID_AND_ATTRIBUTES {
            Sid: sid.0,
            Attributes: SE_GROUP_ENABLED as u32,
        })
        .collect();
    let mut security = SECURITY_CAPABILITIES {
        AppContainerSid: sid.0,
        Capabilities: capabilities.as_mut_ptr(),
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
        // SAFETY: the worker's handles, closed once, here.
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
    let buffer = crate::state::owner::token(TokenUser)
        .map_err(|err| format!("cannot read this user: {err}"))?;
    // SAFETY: TOKEN_USER starts the buffer, which `token` filled.
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

/// Puts the launcher `process` (`pid`), started suspended, in the host's
/// own job, so that it, and the worker it starts, end when the host does,
/// however the host ends; then lets it run. One that cannot join the job is
/// ended before it runs at all: no worker outlives the host.
pub fn tie_to_host(process: HANDLE, pid: u32) -> Result<(), String> {
    static HOST_JOB: OnceLock<Result<usize, String>> = OnceLock::new();
    let job = HOST_JOB.get_or_init(|| {
        Job::new().map(|job| {
            let handle = job.0 as usize;
            // Kept for the host's whole life.
            std::mem::forget(job);
            handle
        })
    });
    let joined = job.clone().and_then(|job| {
        // SAFETY: the host's job and a live launcher of its own.
        match unsafe { AssignProcessToJobObject(job as HANDLE, process) } {
            0 => Err(format!(
                "cannot put the worker in the host's job: {}",
                std::io::Error::last_os_error()
            )),
            _ => Ok(()),
        }
    });
    let started = joined.and_then(|()| resume(pid));
    if started.is_err() {
        // SAFETY: ends a launcher of ours that has not run.
        unsafe { TerminateProcess(process, 1) };
    }
    started
}

/// Lets the suspended process `pid` run: its thread resumed (a process
/// started suspended has one).
fn resume(pid: u32) -> Result<(), String> {
    let failed = |what: &str| format!("{what}: {}", std::io::Error::last_os_error());
    // SAFETY: a snapshot of every thread, closed below.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(failed("cannot find the worker's thread"));
    }
    // SAFETY: zeroed is a valid THREADENTRY32 once its size is set.
    let mut entry: THREADENTRY32 = unsafe { std::mem::zeroed() };
    entry.dwSize = size_of::<THREADENTRY32>() as u32;
    let mut resumed = 0;
    // SAFETY: `entry` is writable for its size; the snapshot is live.
    let mut more = unsafe { Thread32First(snapshot, &mut entry) } != 0;
    while more {
        if entry.th32OwnerProcessID == pid {
            // SAFETY: a thread of the worker, closed below.
            let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
            if !thread.is_null() {
                // SAFETY: a live thread handle, closed once.
                unsafe {
                    if ResumeThread(thread) != u32::MAX {
                        resumed += 1;
                    }
                    CloseHandle(thread);
                }
            }
        }
        // SAFETY: as above.
        more = unsafe { Thread32Next(snapshot, &mut entry) } != 0;
    }
    // SAFETY: the snapshot, closed once.
    unsafe { CloseHandle(snapshot) };
    match resumed {
        0 => Err("cannot start the worker: its thread was not resumed".to_string()),
        _ => Ok(()),
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
    // SAFETY: closed once, here.
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;

    use super::*;
    use windows_sys::Win32::Security::DeriveCapabilitySidsFromName;

    /// Test state lives under the repo's gitignored `.dev/`, like every dev run.
    fn test_dir() -> tempfile::TempDir {
        let parent = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.dev/tests");
        fs::create_dir_all(&parent).unwrap();
        tempfile::tempdir_in(parent).unwrap()
    }

    /// A folder in home, where a call's reads are granted.
    fn home_dir() -> tempfile::TempDir {
        tempfile::tempdir_in(std::env::home_dir().unwrap()).unwrap()
    }

    fn policy(root: &Path) -> Policy {
        Policy {
            areas: vec![root.join("area")],
            reads: vec![],
            hidden: crate::sandbox::never_readable(&std::env::home_dir().unwrap()),
            state_dir: root.join("state"),
            exec: true,
            home: None,
            network: false,
            tmpdir: Some(root.join("tmp")),
            kill_after: None,
            program: None,
            workdir: None,
            break_for_test: false,
        }
    }

    #[test]
    fn a_launcher_runs_only_once_it_is_in_the_hosts_job() {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::CREATE_SUSPENDED;
        let start = || {
            Command::new("cmd")
                .args(["/c", "exit 7"])
                .creation_flags(CREATE_SUSPENDED)
                .spawn()
                .unwrap()
        };
        let mut ran = start();
        tie_to_host(ran.as_raw_handle(), ran.id()).unwrap();
        assert_eq!(ran.wait().unwrap().code(), Some(7));
        // One that cannot be started is ended before it runs at all.
        let mut held = start();
        assert!(tie_to_host(held.as_raw_handle(), u32::MAX).is_err());
        assert_eq!(held.wait().unwrap().code(), Some(1));
    }

    #[test]
    fn paths_match_as_windows_names_them() {
        assert!(within(
            Path::new(r"\\?\C:\Users\Me\AppData\x"),
            Path::new(r"c:\users\me\appdata")
        ));
        assert!(within(
            Path::new(r"C:\Users\Me"),
            Path::new(r"\\?\C:\Users\Me")
        ));
        assert!(!within(
            Path::new(r"C:\Users\Me2"),
            Path::new(r"C:\Users\Me")
        ));
        assert!(!within(Path::new(r"C:\Users"), Path::new(r"C:\Users\Me")));
    }

    #[test]
    fn a_program_every_container_runs_needs_no_grant() {
        let system = std::env::var_os("SystemRoot").unwrap();
        assert!(every_container_runs(
            &Path::new(&system).join(r"System32\cmd.exe")
        ));
        let dir = test_dir();
        let own = dir.path().join("own.exe");
        fs::write(&own, "").unwrap();
        assert!(!every_container_runs(&own));
    }

    #[test]
    fn only_a_protected_list_keeps_a_grant_out() {
        let dir = test_dir();
        let root = dir.path().canonicalize().unwrap();
        let open = root.join("open");
        fs::create_dir(&open).unwrap();
        assert!(!sealed(&open).unwrap());
        let private = root.join("private");
        fs::create_dir(&private).unwrap();
        crate::state::owner::folder(&private).unwrap();
        assert!(sealed(&private).unwrap());
        // Protected, but open to every container: not hidden.
        let shared = root.join("shared");
        fs::create_dir(&shared).unwrap();
        crate::state::owner::folder(&shared).unwrap();
        let every = Sid::from_string("S-1-15-2-1").unwrap();
        let grant = Grant {
            path: shared.clone(),
            access: Access::Read,
        };
        set_entry(&grant, every.0).unwrap();
        assert!(!sealed(&shared).unwrap());
    }

    #[test]
    fn no_write_grant_holds_the_state_even_sealed() {
        let dir = home_dir();
        let root = dir.path().canonicalize().unwrap();
        for name in ["area/parent/state", "tmp"] {
            fs::create_dir_all(root.join(name)).unwrap();
        }
        crate::state::owner::folder(&root.join("area/parent/state")).unwrap();
        let mut held = policy(&root);
        held.state_dir = root.join("area/parent/state");
        let failure = grants(&held).err().unwrap();
        assert!(failure.contains("which no worker may move"), "{failure}");
        // Read, it must be sealed, which it is.
        held.areas = vec![];
        held.reads = vec![root.join("area")];
        grants(&held).unwrap();
    }

    #[test]
    fn a_hidden_folder_inside_a_read_grant_must_keep_it_out() {
        let dir = home_dir();
        let root = dir.path().canonicalize().unwrap();
        for name in ["area/state", "tmp"] {
            fs::create_dir_all(root.join(name)).unwrap();
        }
        let mut read = policy(&root);
        read.areas = vec![];
        read.reads = vec![root.join("area")];
        read.state_dir = root.join("area/state");
        let failure = grants(&read).err().unwrap();
        assert!(
            failure.contains("is hidden but inside a folder"),
            "{failure}"
        );
        crate::state::owner::folder(&root.join("area/state")).unwrap();
        grants(&read).unwrap();
    }

    #[test]
    fn no_write_grant_holds_the_latch_it_runs() {
        let dir = test_dir();
        fs::create_dir_all(dir.path().join("tmp")).unwrap();
        let program = std::env::current_exe().unwrap();
        let mut held = policy(dir.path());
        held.state_dir = dir.path().join("state");
        held.areas = vec![program.parent().unwrap().to_path_buf()];
        let failure = grants(&held).err().unwrap();
        assert!(failure.contains("which no worker may move"), "{failure}");
        // Reading it is another matter.
        held.areas = vec![];
        held.reads = vec![program.parent().unwrap().to_path_buf()];
        grants(&held).unwrap();
    }

    /// In a real container: an exec worker reads and writes its area, and
    /// nothing hidden: not the state, not AppData, and it never writes the
    /// latch it runs or its folder. A sealed folder in its area keeps what is
    /// in it, but the worker can still move it by renaming the folder above
    /// and put another in its place: why no write grant holds the state.
    /// Needs a desktop to start a container in (run it from a signed-in
    /// session).
    #[test]
    fn a_container_reaches_its_grants_and_nothing_hidden() {
        if let Ok(root) = std::env::var("LATCH_CONTAINER_TEST") {
            let root = PathBuf::from(root);
            let appdata = PathBuf::from(std::env::var("LATCH_CONTAINER_APPDATA").unwrap());
            confine(&policy(&root)).unwrap();
            let program = std::env::current_exe().unwrap();
            let checks = [
                ("area read", fs::read(root.join("area/inside")).is_ok()),
                (
                    "area write",
                    fs::write(root.join("area/new"), "new").is_ok(),
                ),
                ("tmp write", fs::write(root.join("tmp/new"), "new").is_ok()),
                ("state read", fs::read(root.join("state/secret")).is_err()),
                ("state list", fs::read_dir(root.join("state")).is_err()),
                (
                    "sealed folder read",
                    fs::read(root.join("area/parent/sealed/secret")).is_err(),
                ),
                (
                    "sealed folder moved by its parent",
                    fs::rename(root.join("area/parent"), root.join("area/moved")).is_ok()
                        && fs::create_dir_all(root.join("area/parent/sealed")).is_ok()
                        && fs::write(root.join("area/parent/sealed/secret"), "planted").is_ok(),
                ),
                (
                    "outside read",
                    fs::read(root.join("outside/other")).is_err(),
                ),
                ("AppData read", fs::read(&appdata).is_err()),
                (
                    "latch written",
                    fs::OpenOptions::new().append(true).open(&program).is_err(),
                ),
                (
                    "latch's folder written",
                    fs::write(program.with_file_name("planted.dll"), "x").is_err(),
                ),
            ];
            let failed: Vec<&str> = checks
                .iter()
                .filter(|(_, held)| !held)
                .map(|(name, _)| *name)
                .collect();
            assert!(failed.is_empty(), "failed in the container: {failed:?}");
            println!("checked in a container");
            return;
        }
        let dir = test_dir();
        let root = dir.path().canonicalize().unwrap();
        for name in ["area/parent/sealed", "state", "tmp", "outside"] {
            fs::create_dir_all(root.join(name)).unwrap();
        }
        fs::write(root.join("area/inside"), "inside").unwrap();
        for sealed in ["state", "area/parent/sealed"] {
            crate::state::owner::folder(&root.join(sealed)).unwrap();
            fs::write(root.join(sealed).join("secret"), "secret").unwrap();
        }
        fs::write(root.join("outside/other"), "other").unwrap();
        // A file of the user's in AppData, which no worker reads.
        let appdata = tempfile::Builder::new()
            .prefix("latch-hidden-")
            .tempfile_in(std::env::var_os("LOCALAPPDATA").unwrap())
            .unwrap();
        fs::write(appdata.path(), "private").unwrap();
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "sandbox::platform::tests::a_container_reaches_its_grants_and_nothing_hidden",
                "--nocapture",
            ])
            .current_dir(root.join("area"))
            .env("LATCH_CONTAINER_TEST", &root)
            .env("LATCH_CONTAINER_APPDATA", appdata.path())
            .output()
            .unwrap();
        assert!(child.status.success(), "{child:?}");
        let said = String::from_utf8_lossy(&child.stdout);
        assert!(said.contains("checked in a container"), "{said}");
        assert!(
            !std::env::current_exe()
                .unwrap()
                .with_file_name("planted.dll")
                .exists()
        );
        // Every grant was taken back.
        assert_eq!(
            fs::read(root.join("area/moved/sealed/secret")).unwrap(),
            b"secret"
        );
        if let Ok(mut left) = fs::read_dir(ledgers(&root.join("state"))) {
            assert!(left.next().is_none(), "a ledger was left");
        }
    }

    #[test]
    fn the_workers_capability_is_the_one_its_name_derives() {
        let name = wide(OsStr::new("latchWorkerVolumeRoot"));
        let (mut groups, mut group_count) = (null_mut(), 0u32);
        let (mut capabilities, mut capability_count) = (null_mut(), 0u32);
        // SAFETY: a NUL-terminated name; Windows allocates the lists.
        let derived = unsafe {
            DeriveCapabilitySidsFromName(
                name.as_ptr(),
                &mut groups,
                &mut group_count,
                &mut capabilities,
                &mut capability_count,
            )
        };
        assert_ne!(derived, 0, "{}", std::io::Error::last_os_error());
        assert_eq!(capability_count, 1);
        let ours = Sid::from_string(VOLUME_ROOT).unwrap();
        // SAFETY: one capability SID at the list's start, live until freed.
        assert_ne!(unsafe { EqualSid(*capabilities, ours.0) }, 0);
        // SAFETY: each SID and both lists are Windows' allocations, freed once.
        unsafe {
            for at in 0..group_count as usize {
                LocalFree((*groups.add(at)).cast());
            }
            for at in 0..capability_count as usize {
                LocalFree((*capabilities.add(at)).cast());
            }
            LocalFree(groups.cast());
            LocalFree(capabilities.cast());
        }
    }

    /// The listing grant with no administrator and no prompt: on a folder
    /// this user owns, as `grant_listing` gives it on each volume's root.
    #[test]
    fn a_listing_grant_is_one_entry_of_root_access_that_nothing_below_inherits() {
        let sid = Sid::from_string(VOLUME_ROOT).unwrap();
        let dir = tempfile::tempdir().unwrap();
        assert!(!admits(dir.path(), &sid).unwrap());
        grant_root(dir.path(), &sid).unwrap();
        assert!(admits(dir.path(), &sid).unwrap());
        // Granted again, nothing changes.
        grant_root(dir.path(), &sid).unwrap();
        assert_eq!(entries_for(dir.path(), &sid), [(0, ROOT_ACCESS)]);
        let inside = dir.path().join("inside");
        std::fs::create_dir(&inside).unwrap();
        std::fs::write(dir.path().join("file"), "").unwrap();
        assert!(entries_for(&inside, &sid).is_empty());
        assert!(entries_for(&dir.path().join("file"), &sid).is_empty());
    }

    /// Each allowing entry `path`'s access list has for `sid`, inherited or
    /// not: its flags and mask.
    fn entries_for(path: &Path, sid: &Sid) -> Vec<(u8, u32)> {
        read_list(path, |_, list| {
            let mut found = Vec::new();
            // SAFETY: the descriptor keeps the list and its entries live.
            for ace in unsafe { entries(list) }? {
                // SAFETY: every entry starts with a header; an allowing entry
                // goes on with its mask and its SID.
                unsafe {
                    let header = *(ace as *const ACE_HEADER);
                    let allowing = ace as *mut ACCESS_ALLOWED_ACE;
                    if header.AceType as u32 == ACCESS_ALLOWED_ACE_TYPE
                        && EqualSid((&raw mut (*allowing).SidStart).cast(), sid.0) != 0
                    {
                        found.push((header.AceFlags, (*allowing).Mask));
                    }
                }
            }
            Ok(found)
        })
        .unwrap()
    }
}

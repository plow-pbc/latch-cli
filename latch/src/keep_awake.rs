//! Keeping this machine awake while its host serves, once its owner turns
//! that on (`latch settings set --keep-awake on`). On macOS the serving host
//! holds one `PreventUserIdleSystemSleep` assertion, in its own process, while
//! the Mac runs on AC power. The display still sleeps and locks as macOS
//! decides, and a closed lid or a low battery still sleeps the Mac. On
//! battery (or a UPS) the host lets go 750 ms after the power changed, so a
//! charger that flickers does not put a long-idle Mac to sleep at once;
//! plugging back in holds again at once; power it cannot read never holds.
//! A refused assertion is a state, not a failure: the host serves on and
//! the setting stays on. On Windows the host's thread requires the system
//! (`SetThreadExecutionState`) as it would hold the assertion, by the same
//! rules: the display still sleeps and locks, and on battery it lets go.
//! Elsewhere it is `unsupported`.
//!
//! The host writes what it holds to `keep-awake.json`, under its run, so
//! `status --local` reads it without asking the host; another run's is
//! ignored.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::failure::Failure;
use crate::lifecycle::{Host, Phase};

const FILE: &str = "keep-awake.json";

/// Why the host holds nothing.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    Disabled,
    NotServing,
    OnBattery,
    Unsupported,
    Refused,
    /// The host cannot tell what powers the machine.
    Unknown,
}

/// `this_machine.hosting.keep_awake`.
#[derive(Serialize, PartialEq, Debug)]
pub struct KeepAwake {
    /// The saved setting.
    pub enabled: bool,
    /// The serving host holds the machine awake now; `null` while it has
    /// not said yet.
    pub held: Option<bool>,
    /// Why not, when it does not; `null` when it does, or has not said.
    pub reason: Option<Reason>,
}

/// What the host holds, as it last wrote it.
#[derive(Serialize, Deserialize)]
struct Observed {
    run: String,
    held: bool,
    reason: Option<Reason>,
}

/// `status --local`'s field for the state folder `dir`, whose host is
/// `host`. Reads only.
pub fn status(dir: &Path, host: &Host) -> Result<KeepAwake, Failure> {
    let enabled = crate::settings::load(dir)?.keep_awake;
    let not = |reason| (Some(false), Some(reason));
    let (held, reason) = if !enabled {
        // A host lets go the moment it reads the change.
        match observed(dir, host) {
            Some(Observed { held: true, .. }) => (Some(true), None),
            _ => not(Reason::Disabled),
        }
    } else if !SUPPORTED {
        not(Reason::Unsupported)
    } else if !host.running || host.state != Phase::Serving {
        not(Reason::NotServing)
    } else {
        match observed(dir, host) {
            Some(observed) => (Some(observed.held), observed.reason),
            None => (None, None),
        }
    };
    Ok(KeepAwake {
        enabled,
        held,
        reason,
    })
}

/// What the running host `host` wrote, if it has.
fn observed(dir: &Path, host: &Host) -> Option<Observed> {
    let bytes = std::fs::read(dir.join(FILE)).ok()?;
    let observed: Observed = serde_json::from_slice(&bytes).ok()?;
    (host.running && host.run.as_ref() == Some(&observed.run)).then_some(observed)
}

/// Holding and letting go as the setting and the power call for; the OS's
/// assertion is given to it, so tests give it a fake.
#[cfg(any(target_os = "macos", windows, test))]
mod keeper {
    use std::time::{Duration, Instant};

    use super::Reason;

    /// How long after the power left AC the host lets go.
    pub const RELEASE_AFTER: Duration = Duration::from_millis(750);

    #[derive(Clone, Copy, PartialEq, Debug)]
    pub enum Power {
        Ac,
        /// Battery or UPS: power that runs out.
        Battery,
        Unknown,
    }

    /// The OS's hold on sleep.
    pub trait Assertion {
        /// Takes it; false if the OS refused.
        fn take(&mut self) -> bool;
        fn release(&mut self);
    }

    /// What one look decided.
    #[derive(PartialEq, Debug)]
    pub struct Look {
        pub held: bool,
        pub reason: Option<Reason>,
        /// When to look again, for a release that is due then.
        pub again: Option<Instant>,
    }

    /// Holds `A` while the setting is on and the power is AC.
    pub struct Keeper<A> {
        pub assertion: A,
        held: bool,
        release_at: Option<Instant>,
    }

    impl<A: Assertion> Keeper<A> {
        pub fn new(assertion: A) -> Keeper<A> {
            Keeper {
                assertion,
                held: false,
                release_at: None,
            }
        }

        /// Holds or lets go as `enabled` and `power` call for at `now`. A
        /// refusal is asked again at the next look.
        pub fn look(&mut self, enabled: bool, power: Power, now: Instant) -> Look {
            let mut refused = false;
            if enabled && power == Power::Ac {
                self.release_at = None;
                if !self.held {
                    self.held = self.assertion.take();
                    refused = !self.held;
                }
            } else if self.held {
                let at = if enabled {
                    *self.release_at.get_or_insert(now + RELEASE_AFTER)
                } else {
                    now
                };
                if now >= at {
                    self.assertion.release();
                    self.held = false;
                    self.release_at = None;
                }
            }
            let reason = match () {
                _ if self.held => None,
                _ if !enabled => Some(Reason::Disabled),
                _ if refused => Some(Reason::Refused),
                _ if power == Power::Unknown => Some(Reason::Unknown),
                _ => Some(Reason::OnBattery),
            };
            Look {
                held: self.held,
                reason,
                again: self.release_at,
            }
        }
    }
}

#[cfg(any(target_os = "macos", windows))]
const SUPPORTED: bool = true;
#[cfg(not(any(target_os = "macos", windows)))]
const SUPPORTED: bool = false;

#[cfg(any(target_os = "macos", windows))]
pub use holding::start;

/// Nothing to hold here: `status` says `unsupported`.
#[cfg(not(any(target_os = "macos", windows)))]
pub struct Holding;

#[cfg(not(any(target_os = "macos", windows)))]
pub fn start(_dir: &Path) -> Holding {
    Holding
}

/// The serving host's hold, alike on macOS and Windows: one thread looks
/// again each time the folder (its settings) or the power changes, and
/// never polls. Only how each OS is asked differs (`platform`).
#[cfg(any(target_os = "macos", windows))]
mod holding {
    use std::path::{Path, PathBuf};
    use std::sync::mpsc::{self, RecvTimeoutError, Sender};
    use std::thread::JoinHandle;
    use std::time::Instant;

    use super::keeper::{Assertion, Keeper, Look};
    use super::platform::{Idle, on_power_change, power};
    use super::{FILE, Observed, Reason};
    use crate::log::log;
    use crate::watch::Watch;

    pub enum Event {
        Look,
        Stop,
    }

    /// The host's hold, from when it starts serving; dropping it lets go.
    pub struct Holding {
        events: Sender<Event>,
        thread: Option<JoinHandle<()>>,
    }

    impl Drop for Holding {
        fn drop(&mut self) {
            let _ = self.events.send(Event::Stop);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    /// Starts holding for the host serving `dir`, and looks again each time
    /// the folder (its settings) or the power changes. It never polls. The
    /// hold is taken and let go on this one thread (Windows' is the
    /// thread's own).
    pub fn start(dir: &Path) -> Holding {
        let (events, woke) = mpsc::channel();
        let dir = dir.to_path_buf();
        on_power_change(events.clone());
        on_folder_change(&dir, events.clone());
        let run = crate::lifecycle::this_run().expect("a serving host has recorded its run");
        let thread = std::thread::spawn(move || {
            let mut keeper = Keeper::new(Idle::default());
            loop {
                let enabled = match crate::settings::load(&dir) {
                    Ok(settings) => settings.keep_awake,
                    Err(failure) => {
                        log(&format!("keep awake: {}", failure.message));
                        false
                    }
                };
                let look = keeper.look(enabled, power(), Instant::now());
                write(&dir, run, &look);
                let event = match look.again {
                    Some(at) => woke.recv_timeout(at.saturating_duration_since(Instant::now())),
                    None => woke.recv().map_err(|_| RecvTimeoutError::Disconnected),
                };
                match event {
                    Ok(Event::Look) | Err(RecvTimeoutError::Timeout) => {}
                    Ok(Event::Stop) | Err(RecvTimeoutError::Disconnected) => break,
                }
            }
            keeper.assertion.release();
            let stopped = Look {
                held: false,
                reason: Some(Reason::NotServing),
                again: None,
            };
            write(&dir, run, &stopped);
        });
        Holding {
            events,
            thread: Some(thread),
        }
    }

    fn write(dir: &Path, run: &str, look: &Look) {
        let observed = Observed {
            run: run.to_string(),
            held: look.held,
            reason: look.reason,
        };
        let json = serde_json::to_string(&observed).expect("an observation serializes") + "\n";
        if let Err(err) = crate::state::write_private(&dir.join(FILE), json.as_bytes()) {
            log(&format!("keep awake: could not write {FILE}: {err}"));
        }
    }

    /// A `Look` each time something in `dir` changes, the settings among it.
    /// The watch is made on its own thread (Windows' holds a handle that
    /// stays there).
    fn on_folder_change(dir: &Path, events: Sender<Event>) {
        let dir = dir.to_path_buf();
        std::thread::spawn(move || {
            let settings: PathBuf = dir.join(crate::settings::FILE);
            let mut watch = match Watch::new(&dir, &[settings], None) {
                Ok(watch) => watch,
                Err(err) => {
                    log(&format!("keep awake: cannot follow the settings: {err}"));
                    return;
                }
            };
            loop {
                if watch.arm().is_err() || events.send(Event::Look).is_err() {
                    return;
                }
                if watch.wait().is_err() {
                    return;
                }
            }
        });
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::{CStr, c_char, c_int, c_void};
    use std::sync::mpsc::Sender;

    use core_foundation::base::{CFRelease, CFTypeRef, TCFType};
    use core_foundation::string::{CFString, CFStringRef};

    use super::holding::Event;
    use super::keeper::{Assertion, Power};
    use crate::log::log;

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOPSCopyPowerSourcesInfo() -> CFTypeRef;
        fn IOPSGetProvidingPowerSourceType(snapshot: CFTypeRef) -> CFStringRef;
        fn IOPMAssertionCreateWithName(
            kind: CFStringRef,
            level: u32,
            name: CFStringRef,
            id: *mut u32,
        ) -> i32;
        fn IOPMAssertionRelease(id: u32) -> i32;
    }

    unsafe extern "C" {
        fn notify_register_file_descriptor(
            name: *const c_char,
            fd: *mut c_int,
            flags: c_int,
            token: *mut c_int,
        ) -> u32;
    }

    /// Posted when what powers the Mac changes (`IOPowerSources.h`).
    const POWER_CHANGED: &CStr = c"com.apple.system.powersources.source";
    /// `kIOPMAssertionLevelOn`.
    const LEVEL_ON: u32 = 255;
    /// The name `pmset -g assertions` and Activity Monitor show.
    const NAME: &str = "Plow Latch: keeping this Mac reachable for your agents";

    /// What powers the Mac now. A Mac with no battery says AC.
    pub fn power() -> Power {
        // SAFETY: IOKit has no Rust API. The snapshot is ours (create
        // rule) and released below; the type string belongs to it (get
        // rule) and is copied out before then.
        unsafe {
            let snapshot = IOPSCopyPowerSourcesInfo();
            if snapshot.is_null() {
                return Power::Unknown;
            }
            let kind = IOPSGetProvidingPowerSourceType(snapshot);
            let kind = (!kind.is_null()).then(|| CFString::wrap_under_get_rule(kind).to_string());
            CFRelease(snapshot);
            match kind.as_deref() {
                Some("AC Power") => Power::Ac,
                Some("Battery Power" | "UPS Power") => Power::Battery,
                _ => Power::Unknown,
            }
        }
    }

    /// An IOPM assertion; it also ends with this process.
    #[derive(Default)]
    pub struct Idle {
        id: Option<u32>,
    }

    impl Assertion for Idle {
        fn take(&mut self) -> bool {
            let kind = CFString::from_static_string("PreventUserIdleSystemSleep");
            let name = CFString::new(NAME);
            let mut id = 0;
            // SAFETY: live CFStrings for the type and name; `id` is
            // written only on success, which is checked.
            let result = unsafe {
                IOPMAssertionCreateWithName(
                    kind.as_concrete_TypeRef(),
                    LEVEL_ON,
                    name.as_concrete_TypeRef(),
                    &mut id,
                )
            };
            if result != 0 {
                log(&format!(
                    "keep awake: macOS refused the assertion ({result:#x})"
                ));
                return false;
            }
            self.id = Some(id);
            true
        }

        fn release(&mut self) {
            if let Some(id) = self.id.take() {
                // SAFETY: an id this process was given and has not released.
                unsafe { IOPMAssertionRelease(id) };
            }
        }
    }

    /// A `Look` each time macOS says the power source changed.
    pub fn on_power_change(events: Sender<Event>) {
        let (mut fd, mut token) = (-1, 0);
        // SAFETY: a live NUL-terminated name and two writable ints; the
        // fd is used only when registration succeeded.
        let status = unsafe {
            notify_register_file_descriptor(POWER_CHANGED.as_ptr(), &mut fd, 0, &mut token)
        };
        if status != 0 {
            log(&format!(
                "keep awake: cannot follow power changes (notify status {status})"
            ));
            return;
        }
        std::thread::spawn(move || {
            let mut posted = [0u8; 4];
            loop {
                // SAFETY: our notify descriptor, never closed; `posted` is
                // writable for its length.
                let read = unsafe { libc::read(fd, posted.as_mut_ptr().cast::<c_void>(), 4) };
                if read <= 0
                    && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
                {
                    return;
                }
                if events.send(Event::Look).is_err() {
                    return;
                }
            }
        });
    }
}

#[cfg(windows)]
mod platform {
    use std::ffi::c_void;
    use std::sync::mpsc::Sender;

    use windows_sys::Win32::System::Power::{
        DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS, ES_CONTINUOUS, ES_SYSTEM_REQUIRED,
        GetSystemPowerStatus, PowerSettingRegisterNotification, SYSTEM_POWER_STATUS,
        SetThreadExecutionState,
    };
    use windows_sys::Win32::System::SystemServices::GUID_ACDC_POWER_SOURCE;
    use windows_sys::Win32::UI::WindowsAndMessaging::DEVICE_NOTIFY_CALLBACK;

    use super::holding::Event;
    use super::keeper::{Assertion, Power};
    use crate::log::log;

    /// `SYSTEM_POWER_STATUS.BatteryFlag`: the PC has no system battery, and
    /// the flag Windows could not read, which has that bit set too.
    const NO_BATTERY: u8 = 128;
    const UNREAD: u8 = 255;

    /// What powers the PC now. A PC with no battery says AC.
    pub fn power() -> Power {
        // SAFETY: zeroed is valid for this plain structure to receive into.
        let mut status: SYSTEM_POWER_STATUS = unsafe { std::mem::zeroed() };
        // SAFETY: `status` is writable for its size.
        if unsafe { GetSystemPowerStatus(&mut status) } == 0 {
            return Power::Unknown;
        }
        // Power Windows cannot read never holds, as on macOS.
        match status.ACLineStatus {
            1 => Power::Ac,
            0 => Power::Battery,
            _ if status.BatteryFlag != UNREAD && status.BatteryFlag & NO_BATTERY != 0 => Power::Ac,
            _ => Power::Unknown,
        }
    }

    /// The hold thread's execution state: Windows does not sleep for idle
    /// while it requires the system, and lets go when it is cleared or the
    /// thread ends. The display still sleeps and locks as Windows decides.
    #[derive(Default)]
    pub struct Idle {
        held: bool,
    }

    impl Assertion for Idle {
        fn take(&mut self) -> bool {
            // SAFETY: flags only, for this thread.
            if unsafe { SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED) } == 0 {
                log("keep awake: Windows refused to keep the system required");
                return false;
            }
            self.held = true;
            true
        }

        fn release(&mut self) {
            if std::mem::take(&mut self.held) {
                // SAFETY: flags only, on the thread that took it.
                unsafe { SetThreadExecutionState(ES_CONTINUOUS) };
            }
        }
    }

    /// Called by Windows when what powers the PC changes.
    unsafe extern "system" fn changed(context: *const c_void, _: u32, _: *const c_void) -> u32 {
        // SAFETY: the Sender `on_power_change` left for the life of the host.
        let events = unsafe { &*context.cast::<Sender<Event>>() };
        let _ = events.send(Event::Look);
        0
    }

    /// A `Look` each time Windows says the power source changed. The
    /// registration, like macOS's, lasts as long as the host.
    pub fn on_power_change(events: Sender<Event>) {
        let context = Box::into_raw(Box::new(events));
        let mut subscribe = DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS {
            Callback: Some(changed),
            Context: context.cast(),
        };
        let mut registration = std::ptr::null_mut();
        // SAFETY: a static GUID, our callback and its context (kept), and a
        // writable handle.
        let status = unsafe {
            PowerSettingRegisterNotification(
                &GUID_ACDC_POWER_SOURCE,
                DEVICE_NOTIFY_CALLBACK,
                (&mut subscribe as *mut DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS).cast(),
                &mut registration,
            )
        };
        if status != 0 {
            log(&format!(
                "keep awake: cannot follow power changes (status {status})"
            ));
            // SAFETY: never registered, so nothing else holds it.
            drop(unsafe { Box::from_raw(context) });
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::keeper::*;
    use super::*;

    /// Grants or refuses as told, and counts what it holds.
    #[derive(Default)]
    struct Fake {
        refuse: bool,
        holding: u32,
    }

    impl Assertion for Fake {
        fn take(&mut self) -> bool {
            if self.refuse {
                return false;
            }
            self.holding += 1;
            true
        }
        fn release(&mut self) {
            self.holding -= 1;
        }
    }

    fn look(held: bool, reason: Option<Reason>, again: Option<Instant>) -> Look {
        Look {
            held,
            reason,
            again,
        }
    }

    #[test]
    fn it_holds_on_ac_lets_go_on_battery_after_the_debounce_and_holds_again_when_plugged_in() {
        let mut keeper = Keeper::new(Fake::default());
        let start = Instant::now();
        assert_eq!(keeper.look(true, Power::Ac, start), look(true, None, None));
        assert_eq!(keeper.assertion.holding, 1);

        // Unplugged: still held until the debounce runs out.
        let due = start + RELEASE_AFTER;
        assert_eq!(
            keeper.look(true, Power::Battery, start),
            look(true, None, Some(due))
        );
        let before = due - Duration::from_millis(1);
        assert_eq!(
            keeper.look(true, Power::Battery, before),
            look(true, None, Some(due))
        );
        assert_eq!(keeper.assertion.holding, 1);
        assert_eq!(
            keeper.look(true, Power::Battery, due),
            look(false, Some(Reason::OnBattery), None)
        );
        assert_eq!(keeper.assertion.holding, 0);

        // Plugged back in: held at once.
        assert_eq!(keeper.look(true, Power::Ac, due), look(true, None, None));
        assert_eq!(keeper.assertion.holding, 1);
    }

    #[test]
    fn a_flicker_within_the_debounce_keeps_the_one_hold() {
        let mut keeper = Keeper::new(Fake::default());
        let start = Instant::now();
        keeper.look(true, Power::Ac, start);
        keeper.look(true, Power::Battery, start);
        let back = start + Duration::from_millis(100);
        assert_eq!(keeper.look(true, Power::Ac, back), look(true, None, None));
        // The old deadline does not let go later.
        let later = start + RELEASE_AFTER;
        assert_eq!(keeper.look(true, Power::Ac, later), look(true, None, None));
        assert_eq!(keeper.assertion.holding, 1);
    }

    #[test]
    fn turning_it_off_lets_go_at_once_and_unknown_power_never_holds() {
        let mut keeper = Keeper::new(Fake::default());
        let start = Instant::now();
        keeper.look(true, Power::Ac, start);
        assert_eq!(
            keeper.look(false, Power::Ac, start),
            look(false, Some(Reason::Disabled), None)
        );
        assert_eq!(keeper.assertion.holding, 0);
        assert_eq!(
            keeper.look(true, Power::Unknown, start),
            look(false, Some(Reason::Unknown), None)
        );
        assert_eq!(keeper.assertion.holding, 0);
    }

    #[test]
    fn a_refusal_is_reported_and_asked_again() {
        let mut keeper = Keeper::new(Fake {
            refuse: true,
            ..Fake::default()
        });
        let start = Instant::now();
        assert_eq!(
            keeper.look(true, Power::Ac, start),
            look(false, Some(Reason::Refused), None)
        );
        keeper.assertion.refuse = false;
        assert_eq!(keeper.look(true, Power::Ac, start), look(true, None, None));
    }

    #[test]
    fn status_reads_only_the_running_hosts_own_observation() {
        let dir = tempfile::tempdir().unwrap();
        let host = |running, run: &str| Host {
            running,
            configuring: false,
            state: if running {
                Phase::Serving
            } else {
                Phase::Stopped
            },
            pid: running.then_some(1),
            run: Some(run.to_string()),
            error: None,
            attached: false,
            waiting: None,
        };
        let status = |host: &Host| status(dir.path(), host).unwrap();
        let shown = |enabled, held, reason| KeepAwake {
            enabled,
            held,
            reason,
        };
        assert_eq!(
            status(&host(true, "1-1")),
            shown(false, Some(false), Some(Reason::Disabled))
        );

        let state = crate::state::StateDir {
            path: dir.path().to_path_buf(),
        };
        crate::settings::set(&state, true).unwrap();
        let held = Observed {
            run: "1-1".into(),
            held: true,
            reason: None,
        };
        std::fs::write(dir.path().join(FILE), serde_json::to_vec(&held).unwrap()).unwrap();
        if !SUPPORTED {
            assert_eq!(
                status(&host(true, "1-1")),
                shown(true, Some(false), Some(Reason::Unsupported))
            );
            return;
        }
        assert_eq!(status(&host(true, "1-1")), shown(true, Some(true), None));
        // Another run's, or a stopped host's, is not this host's hold.
        assert_eq!(status(&host(true, "2-2")), shown(true, None, None));
        assert_eq!(
            status(&host(false, "1-1")),
            shown(true, Some(false), Some(Reason::NotServing))
        );
    }
}

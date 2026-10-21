//! `latch keychain-probe`: whether this build may keep secrets in the
//! data-protection Keychain under an access group. It stores one throwaway
//! item, reads it back, deletes it, and prints one JSON line naming the step
//! that failed and its status. It touches no other item.
//!
//! Only a build signed with the group in its entitlements, under a
//! provisioning profile that allows them, can pass: any other stops at the
//! first step with -34018 (`errSecMissingEntitlement`). The release workflow
//! runs it from the signed helper bundle, through a link and from launchd.

use std::process::ExitCode;

use serde_json::{Value, json};

pub fn run(group: &str) -> ExitCode {
    let mut report = platform::probe(group);
    report["group"] = json!(group);
    report["exe"] = json!(std::env::current_exe().ok());
    report["argv0"] = json!(std::env::args().next());
    let ok = report["ok"] == true;
    println!("{report}");
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn failed(step: &str, status: i32, message: Option<String>) -> Value {
    json!({ "ok": false, "step": step, "status": status, "message": message })
}

#[cfg(target_os = "macos")]
mod platform {
    use security_framework::access_control::{ProtectionMode, SecAccessControl};
    use security_framework::base::Error;
    use security_framework::passwords::{
        PasswordOptions, delete_generic_password_options, generic_password,
        set_generic_password_options,
    };
    use serde_json::{Value, json};

    use super::failed;

    const SERVICE: &str = "co.plow.latch.keychain-probe";
    /// `errSecItemNotFound`: what a read of the deleted item must say.
    const NOT_FOUND: i32 = -25300;

    fn item(group: &str, account: &str) -> PasswordOptions {
        let mut options = PasswordOptions::new_generic_password(SERVICE, account);
        options.use_protected_keychain();
        options.set_access_group(group);
        options
    }

    fn step_failed(step: &str, err: Error) -> Value {
        failed(step, err.code(), err.message())
    }

    pub fn probe(group: &str) -> Value {
        let account = format!("probe-{:016x}", rand::random::<u64>());
        let secret = format!("{:032x}", rand::random::<u128>());

        // Readable after the first unlock since boot, so a background host
        // can read it with the screen locked; never synced, no user presence.
        let mut store = item(group, &account);
        match SecAccessControl::create_with_protection(
            Some(ProtectionMode::AccessibleAfterFirstUnlockThisDeviceOnly),
            0,
        ) {
            Ok(access) => store.set_access_control(access),
            Err(err) => return step_failed("access_control", err),
        }
        if let Err(err) = set_generic_password_options(secret.as_bytes(), store) {
            return step_failed("store", err);
        }

        // Delete whatever the read finds, so a failed comparison leaves
        // nothing behind either.
        let read = generic_password(item(group, &account));
        let deleted = delete_generic_password_options(item(group, &account));
        match read {
            Err(err) => return step_failed("read", err),
            Ok(bytes) if bytes != secret.as_bytes() => {
                return failed("compare", 0, Some("read back a different value".into()));
            }
            Ok(_) => {}
        }
        if let Err(err) = deleted {
            return step_failed("delete", err);
        }
        match generic_password(item(group, &account)) {
            Err(err) if err.code() == NOT_FOUND => json!({ "ok": true }),
            Err(err) => step_failed("gone", err),
            Ok(_) => failed("gone", 0, Some("the item is still there".into())),
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use serde_json::Value;

    pub fn probe(_group: &str) -> Value {
        super::failed("platform", 0, Some("the Keychain is macOS only".into()))
    }
}

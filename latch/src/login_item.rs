//! "Start when you log in": the launch agent in the app's bundle
//! (Contents/Library/LaunchAgents/ID.login.plist) that the app's window
//! registers with ServiceManagement. macOS keeps it for the bundle where it
//! was turned on, so when the installer moves the app to another folder and
//! removes the old copy, the new copy registers it again: `latch login-item`
//! (its status) before the move, `latch login-item --again` after it.

use objc2::msg_send;
use objc2::runtime::{AnyClass, AnyObject};
use serde_json::{Value, json};

use crate::failure::{Failure, code};

const SERVICE_MANAGEMENT: &std::ffi::CStr =
    c"/System/Library/Frameworks/ServiceManagement.framework/ServiceManagement";

/// The login item's status, as ServiceManagement reports it for this app.
pub fn status() -> Result<Value, Failure> {
    objc2::rc::autoreleasepool(|_| {
        let service = service()?;
        Ok(json!({"login_item": status_of(service)}))
    })
}

/// Registers the login item again from this app's bundle: off, then on.
pub fn again() -> Result<Value, Failure> {
    objc2::rc::autoreleasepool(|_| {
        let service = service()?;
        let mut error: *mut AnyObject = std::ptr::null_mut();
        // SAFETY: a live SMAppService; an unregister that fails because it
        // was not registered here leaves nothing to undo.
        let _: bool = unsafe { msg_send![service, unregisterAndReturnError: &mut error] };
        error = std::ptr::null_mut();
        // SAFETY: as above; `error` receives an autoreleased NSError or stays null.
        let registered: bool = unsafe { msg_send![service, registerAndReturnError: &mut error] };
        let status = status_of(service);
        if !registered && status != "requires_approval" {
            return Err(failed(&format!(
                "macOS did not register it ({})",
                describe(error)
            )));
        }
        Ok(json!({"login_item": status}))
    })
}

/// This app's login item, `SMAppService.agent(plistName: "ID.login.plist")`,
/// autoreleased: alive until the enclosing pool drains.
fn service() -> Result<*mut AnyObject, Failure> {
    let id = crate::gui::bundle_id().ok_or_else(|| failed("this latch is not in an app bundle"))?;
    // SAFETY: a NUL-terminated path; the framework stays loaded once loaded.
    if unsafe { libc::dlopen(SERVICE_MANAGEMENT.as_ptr(), libc::RTLD_LAZY) }.is_null() {
        return Err(failed("ServiceManagement could not load"));
    }
    let class = AnyClass::get(c"SMAppService").ok_or_else(|| failed("no SMAppService"))?;
    let name = ns_string(&format!("{id}.login.plist"))?;
    // SAFETY: a class method that takes an NSString and returns an
    // autoreleased SMAppService, alive for the enclosing pool.
    let service: *mut AnyObject = unsafe { msg_send![class, agentServiceWithPlistName: name] };
    if service.is_null() {
        return Err(failed("no login item in this app"));
    }
    Ok(service)
}

/// `service`: a live SMAppService.
fn status_of(service: *mut AnyObject) -> &'static str {
    // SAFETY: `status` is an NSInteger property of SMAppService.
    let status: isize = unsafe { msg_send![service, status] };
    match status {
        0 => "not_registered",
        1 => "enabled",
        2 => "requires_approval",
        _ => "not_found",
    }
}

/// An autoreleased NSString, alive until the enclosing pool drains.
fn ns_string(text: &str) -> Result<*mut AnyObject, Failure> {
    let class = AnyClass::get(c"NSString").ok_or_else(|| failed("no NSString"))?;
    let text = std::ffi::CString::new(text).map_err(|_| failed("a name with a NUL in it"))?;
    // SAFETY: a NUL-terminated UTF-8 string; the result is autoreleased.
    let string: *mut AnyObject = unsafe { msg_send![class, stringWithUTF8String: text.as_ptr()] };
    if string.is_null() {
        return Err(failed("could not make an NSString"));
    }
    Ok(string)
}

/// An NSError's description, or "no reason given".
fn describe(error: *mut AnyObject) -> String {
    // SAFETY: null, or an autoreleased NSError still alive in the pool.
    let Some(error) = (unsafe { error.as_ref() }) else {
        return "no reason given".into();
    };
    // SAFETY: NSError's localizedDescription is an NSString; UTF8String
    // points into it while it lives.
    let text: *const std::ffi::c_char = unsafe {
        let description: *mut AnyObject = msg_send![error, localizedDescription];
        msg_send![description, UTF8String]
    };
    if text.is_null() {
        return "no reason given".into();
    }
    // SAFETY: a NUL-terminated string owned by the description.
    unsafe { std::ffi::CStr::from_ptr(text) }
        .to_string_lossy()
        .into_owned()
}

fn failed(why: &str) -> Failure {
    Failure::new(code::app_broken, format!("the login item: {why}"))
}

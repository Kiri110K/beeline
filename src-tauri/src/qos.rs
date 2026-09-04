//! Thread QoS control, shared by every background worker (SPEC §10: heavy work runs
//! at background QoS so it never competes with the UI). Extracted from the Name Index
//! crawl so the operations engine reuses the same libSystem shim rather than a copy.

#[cfg(target_os = "macos")]
use objc2::{rc::Retained, runtime::ProtocolObject};
#[cfg(target_os = "macos")]
use objc2_foundation::{NSActivityOptions, NSObjectProtocol, NSProcessInfo, NSString};

/// A short process-level activity that prevents App Nap from stretching a bounded startup
/// task. It still allows idle system sleep and ends automatically when dropped.
pub struct ProcessActivity {
    #[cfg(target_os = "macos")]
    process: Retained<NSProcessInfo>,
    #[cfg(target_os = "macos")]
    token: Retained<ProtocolObject<dyn NSObjectProtocol>>,
}

impl Drop for ProcessActivity {
    fn drop(&mut self) {
        #[cfg(target_os = "macos")]
        // SAFETY: `token` was returned by this exact process object's beginActivity call
        // and remains retained until after endActivity returns.
        unsafe {
            self.process.endActivity(&self.token);
        }
    }
}

pub fn begin_bounded_startup_activity(reason: &str) -> ProcessActivity {
    #[cfg(target_os = "macos")]
    {
        let process = NSProcessInfo::processInfo();
        let reason = NSString::from_str(reason);
        let token = process.beginActivityWithOptions_reason(
            NSActivityOptions::UserInitiatedAllowingIdleSystemSleep,
            &reason,
        );
        ProcessActivity { process, token }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = reason;
        ProcessActivity {}
    }
}

/// Set the calling thread's QoS on macOS. This links `libSystem` (always present);
/// no crate dependency is needed. Darwin's classes used here are background `0x09`,
/// utility `0x11`, and user-initiated `0x19`.
pub fn set_qos(qos_class: u32) {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: `pthread_set_qos_class_self_np` is a libSystem C function that only
        // reads its two scalar arguments and adjusts the current thread's QoS.
        unsafe extern "C" {
            fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: i32) -> i32;
        }
        // SAFETY: see above — plain scalar call, no memory is touched.
        unsafe {
            pthread_set_qos_class_self_np(qos_class, 0);
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = qos_class;
}

/// Background QoS (`0x09`): darwin's heaviest throttle, for rare small bursts of work
/// where slowing down costs nothing. Batch file operations and FSEvents increments use
/// this so they yield the machine to the UI (SPEC §10).
pub fn set_background_qos() {
    set_qos(0x09);
}

/// User-initiated QoS (`0x19`): work triggered directly by an interaction whose result the
/// user is waiting for. Search scans use this so a process waking from its hidden state does
/// not run the first keystrokes at background priority.
pub fn set_user_initiated_qos() {
    set_qos(0x19);
}

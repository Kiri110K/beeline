//! Event-driven macOS power-source state for the Name Index energy policy (SPEC §10).
//!
//! The monitor installs an IOKit run-loop source on its own sleeping thread. There is no
//! polling timer: the thread wakes only when macOS reports that power-source information
//! changed. This keeps the hidden resident at zero periodic work.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerSource {
    External,
    Battery,
    Unknown,
}

impl PowerSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::External => "external",
            Self::Battery => "battery",
            Self::Unknown => "unknown",
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::{
        ffi::c_void,
        panic::{catch_unwind, AssertUnwindSafe},
        ptr::NonNull,
        sync::Mutex,
        thread,
    };

    use objc2_core_foundation::{
        kCFRunLoopDefaultMode, CFRetained, CFRunLoop, CFRunLoopSource, CFString, CFType,
    };

    use super::PowerSource;

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOPSCopyPowerSourcesInfo() -> *const CFType;
        fn IOPSGetProvidingPowerSourceType(snapshot: *const CFType) -> *const CFString;
        fn IOPSNotificationCreateRunLoopSource(
            callback: unsafe extern "C" fn(*mut c_void),
            context: *mut c_void,
        ) -> *const CFRunLoopSource;
    }

    pub fn current() -> PowerSource {
        // SAFETY: IOKit returns a +1 Core Foundation object under the Copy rule. CFRetained
        // owns that reference and releases it after the borrowed provider string is read.
        let Some(snapshot_ptr) = NonNull::new(unsafe { IOPSCopyPowerSourcesInfo() }.cast_mut())
        else {
            return PowerSource::Unknown;
        };
        let snapshot = unsafe { CFRetained::<CFType>::from_raw(snapshot_ptr) };
        // SAFETY: `snapshot` is a valid object from IOPSCopyPowerSourcesInfo and remains
        // alive while the Get-rule string is borrowed.
        let provider = unsafe {
            IOPSGetProvidingPowerSourceType(CFRetained::as_ptr(&snapshot).as_ptr()).as_ref()
        };
        match provider.map(ToString::to_string).as_deref() {
            Some("AC Power") => PowerSource::External,
            Some("Battery Power") => PowerSource::Battery,
            _ => PowerSource::Unknown,
        }
    }

    type Callback = Box<dyn Fn(PowerSource) + Send + Sync + 'static>;

    struct MonitorContext {
        callback: Callback,
        last: Mutex<Option<PowerSource>>,
    }

    impl MonitorContext {
        fn emit_if_changed(&self) {
            let source = current();
            let changed = {
                let mut last = self.last.lock().expect("power source lock poisoned");
                if *last == Some(source) {
                    false
                } else {
                    *last = Some(source);
                    true
                }
            };
            if changed {
                (self.callback)(source);
            }
        }
    }

    unsafe extern "C" fn power_source_changed(context: *mut c_void) {
        // A panic must never unwind through IOKit's C callback boundary. The context remains
        // owned by the monitor thread for at least as long as its run-loop source is active.
        let _ = catch_unwind(AssertUnwindSafe(|| {
            if let Some(context) = unsafe { context.cast::<MonitorContext>().as_ref() } {
                context.emit_if_changed();
            }
        }));
    }

    pub fn spawn_monitor(callback: Callback) {
        thread::spawn(move || {
            crate::qos::set_background_qos();
            let context = Box::into_raw(Box::new(MonitorContext {
                callback,
                last: Mutex::new(None),
            }));

            // Emit the initial state so telemetry and the Junk queue share one source of
            // truth before the first physical power transition.
            unsafe { &*context }.emit_if_changed();

            // SAFETY: the callback ABI matches IOPowerSourceCallbackType and `context` stays
            // allocated until the run loop exits and the source can no longer invoke it.
            let source_ptr = unsafe {
                IOPSNotificationCreateRunLoopSource(power_source_changed, context.cast())
            };
            let Some(source_ptr) = NonNull::new(source_ptr.cast_mut()) else {
                // SAFETY: source creation failed, so IOKit never retained or scheduled the
                // context pointer.
                drop(unsafe { Box::from_raw(context) });
                return;
            };
            // SAFETY: CreateRunLoopSource follows the Create rule and returns +1.
            let source = unsafe { CFRetained::<CFRunLoopSource>::from_raw(source_ptr) };
            let Some(run_loop) = CFRunLoop::current() else {
                drop(unsafe { Box::from_raw(context) });
                return;
            };
            let mode = unsafe { kCFRunLoopDefaultMode };
            run_loop.add_source(Some(&source), mode);
            // Re-read after registration to close the small gap between the initial sample
            // and installing the notification source. The changed-only guard deduplicates
            // the ordinary case.
            unsafe { &*context }.emit_if_changed();
            CFRunLoop::run();

            // Detach the source before destroying its context so no later run-loop pass can
            // invoke the callback with a freed pointer.
            run_loop.remove_source(Some(&source), mode);
            drop(unsafe { Box::from_raw(context) });
        });
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::PowerSource;

    pub fn current() -> PowerSource {
        PowerSource::External
    }

    pub fn spawn_monitor(callback: Box<dyn Fn(PowerSource) + Send + Sync + 'static>) {
        callback(PowerSource::External);
    }
}

pub fn current_source() -> PowerSource {
    platform::current()
}

pub fn spawn_monitor(callback: impl Fn(PowerSource) + Send + Sync + 'static) {
    platform::spawn_monitor(Box::new(callback));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_power_source_is_known() {
        assert_ne!(current_source(), PowerSource::Unknown);
    }
}

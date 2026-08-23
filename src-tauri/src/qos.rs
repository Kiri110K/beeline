//! Thread QoS control, shared by every background worker (SPEC §10: heavy work runs
//! at background QoS so it never competes with the UI). Extracted from the Name Index
//! crawl so the operations engine reuses the same libSystem shim rather than a copy.

/// Lower the calling thread's QoS on macOS. This links `libSystem` (always present);
/// no crate dependency is needed. `QOS_CLASS_BACKGROUND` is `0x09`,
/// `QOS_CLASS_UTILITY` is `0x11`.
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

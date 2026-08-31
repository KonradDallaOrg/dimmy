//! Keep Windows from parking local inference on the efficiency cores.
//!
//! Windows applies EcoQoS to processes it judges to be background, which is
//! what Dimmy is almost all of the time: a tray app whose only window is a
//! small always-on-top pill. A throttled process is scheduled onto the E-cores
//! at reduced clock, and the cost lands hardest on exactly the shape of work
//! the STT path does — many short inference calls in a row rather than one
//! long compute block.
//!
//! Measured on an i7-12700H against the shipped `gtcrn_simple.onnx`, driving
//! the same per-frame loop `gtcrn::process` uses:
//!
//! ```text
//! throttled       5.72 ms/frame
//! not throttled   1.02 ms/frame     5.6x
//! ```
//!
//! and end-to-end in the app, 25 s of dictation, same build and same audio:
//! 9106 ms of denoise throttled against 1443 ms not throttled. The user-visible
//! symptom was a dictation that took 24 s to come back instead of 3.
//!
//! The opt-out is scoped rather than set once at startup, deliberately. Dimmy
//! sits idle almost all day and still costs ~10% of a core doing so; leaving it
//! permanently exempt would move that idle cost onto the P-cores and charge the
//! battery for nothing. Held only around inference, the exemption covers the
//! window where latency is visible and gives the scheduler its say everywhere
//! else.
//!
//! Scoping is only viable because the transition is immediate. Measured: the
//! `SetProcessInformation` call takes 0.090 ms and the very NEXT frame already
//! runs at full speed, with no ramp. Were it lazy, a guard taken at the start
//! of a 1.4 s job would expire before it helped.
//!
//! Non-Windows targets get a guard that does nothing, so call sites stay free
//! of `cfg`.

use std::sync::atomic::{AtomicUsize, Ordering};

/// Nesting depth. The guard is taken by the leaf inference functions, which do
/// not currently nest, but a future caller wrapping a whole job would otherwise
/// have the inner guard's `Drop` re-arm throttling underneath it. Counting is
/// ten lines and removes the failure mode rather than documenting it.
static DEPTH: AtomicUsize = AtomicUsize::new(0);

/// Exempts the process from EcoQoS for as long as it is alive.
///
/// Take one around any stretch of local CPU inference:
///
/// ```ignore
/// let _no_throttle = win_qos::NoThrottle::for_local_inference();
/// ```
#[must_use = "the exemption lasts only as long as the guard is alive; \
              binding to `_` drops it immediately"]
pub struct NoThrottle {
    _private: (),
}

impl NoThrottle {
    pub fn for_local_inference() -> Self {
        if DEPTH.fetch_add(1, Ordering::SeqCst) == 0 {
            set_throttling(Throttling::Exempt);
        }
        Self { _private: () }
    }
}

impl Drop for NoThrottle {
    fn drop(&mut self) {
        let previous = DEPTH.fetch_sub(1, Ordering::SeqCst);
        assert!(
            previous > 0,
            "NoThrottle depth underflow: a guard was dropped without a matching \
             construction, so the process would be left permanently exempt"
        );
        if previous == 1 {
            set_throttling(Throttling::SystemManaged);
        }
    }
}

enum Throttling {
    /// Never throttle this process, whatever Windows thinks of its windows.
    Exempt,
    /// Hand the decision back to Windows. This is the state a process starts
    /// in, NOT "always throttle" — restoring it must not be confused with
    /// forcing throttling on.
    SystemManaged,
}

#[cfg(target_os = "windows")]
fn set_throttling(mode: Throttling) {
    use windows::Win32::System::Threading::{
        GetCurrentProcess, ProcessPowerThrottling, SetProcessInformation,
        PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        PROCESS_POWER_THROTTLING_STATE,
    };

    // ControlMask names the policy we are taking over; StateMask is the value
    // we want for it. Handing the decision back means clearing BOTH — leaving
    // ControlMask set with StateMask clear is a permanent exemption, not a
    // restore.
    let (control, state) = match mode {
        Throttling::Exempt => (PROCESS_POWER_THROTTLING_EXECUTION_SPEED, 0),
        Throttling::SystemManaged => (0, 0),
    };
    let info = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: control,
        StateMask: state,
    };

    // Deliberately not an assert. This is a scheduling hint: on a Windows that
    // predates the API, or under a policy that forbids it, the call fails and
    // the app is merely as slow as it was before. Taking the process down over
    // a hint would be a worse outcome than the latency it exists to fix.
    let rc = unsafe {
        SetProcessInformation(
            GetCurrentProcess(),
            ProcessPowerThrottling,
            std::ptr::addr_of!(info) as *const std::ffi::c_void,
            std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        )
    };
    if let Err(e) = rc {
        crate::log(&format!("[QoS] SetProcessInformation failed: {e}"));
    }
}

#[cfg(not(target_os = "windows"))]
fn set_throttling(_mode: Throttling) {
    // macOS and Linux have no equivalent of EcoQoS demoting a background
    // process onto slower cores, so there is nothing to opt out of.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn depth_returns_to_zero_when_guards_are_dropped() {
        let before = DEPTH.load(Ordering::SeqCst);
        {
            let _a = NoThrottle::for_local_inference();
            assert_eq!(DEPTH.load(Ordering::SeqCst), before + 1);
        }
        assert_eq!(
            DEPTH.load(Ordering::SeqCst),
            before,
            "a dropped guard must release its depth, otherwise the process \
             stays exempt forever"
        );
    }

    #[test]
    fn nested_guards_keep_the_exemption_until_the_outermost_is_dropped() {
        // The regression this counter exists for: without it the inner drop
        // would re-arm throttling while the outer scope still needs it off.
        let before = DEPTH.load(Ordering::SeqCst);
        let outer = NoThrottle::for_local_inference();
        {
            let _inner = NoThrottle::for_local_inference();
            assert_eq!(DEPTH.load(Ordering::SeqCst), before + 2);
        }
        assert_eq!(
            DEPTH.load(Ordering::SeqCst),
            before + 1,
            "the outer guard must still hold the exemption"
        );
        drop(outer);
        assert_eq!(DEPTH.load(Ordering::SeqCst), before);
    }

    /// Read back what Windows actually believes about this process. The unit
    /// tests above only prove our own bookkeeping; this one proves the syscall
    /// landed. Without it, a wrong `ControlMask`/`StateMask` pair or a silently
    /// failing call would leave every assertion green and every dictation slow.
    #[cfg(target_os = "windows")]
    fn read_back() -> (u32, u32) {
        use windows::Win32::System::Threading::{
            GetCurrentProcess, GetProcessInformation, ProcessPowerThrottling,
            PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_STATE,
        };
        let mut info = PROCESS_POWER_THROTTLING_STATE {
            Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
            ControlMask: 0,
            StateMask: 0,
        };
        unsafe {
            GetProcessInformation(
                GetCurrentProcess(),
                ProcessPowerThrottling,
                std::ptr::addr_of_mut!(info) as *mut std::ffi::c_void,
                std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
            )
            .expect("GetProcessInformation(ProcessPowerThrottling) must succeed");
        }
        (info.ControlMask, info.StateMask)
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn the_guard_actually_changes_what_windows_thinks() {
        use windows::Win32::System::Threading::PROCESS_POWER_THROTTLING_EXECUTION_SPEED;

        assert_eq!(
            DEPTH.load(Ordering::SeqCst),
            0,
            "another test is holding a guard; this one must observe a clean process"
        );
        {
            let _g = NoThrottle::for_local_inference();
            let (control, state) = read_back();
            assert_eq!(
                control, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
                "we must have taken over the EXECUTION_SPEED policy"
            );
            assert_eq!(
                state, 0,
                "EXECUTION_SPEED must be OFF, i.e. do not throttle; a set bit here                  would FORCE the E-cores instead of avoiding them"
            );
        }
        let (control, state) = read_back();
        assert_eq!(
            (control, state),
            (0, 0),
            "dropping the guard must hand the decision back to Windows, not leave              the process permanently exempt"
        );
    }

    #[test]
    fn taking_and_dropping_a_guard_does_not_panic_on_any_platform() {
        // The Windows path issues a real syscall and the others are a no-op;
        // both must be safe to call from a test process.
        for _ in 0..3 {
            let _g = NoThrottle::for_local_inference();
        }
    }
}

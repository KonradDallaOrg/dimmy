import Foundation

// MARK: - DimmyCore — call-detect FFI wrappers
//
// Thin Swift bridges around the Rust call-detector state machine.
// CallDetectionManager pumps observations via `callSignal*`; the
// CallNudgeWindowController dispatches user responses via
// `callSignalResponse`. The Rust state machine (call_detector.rs) owns
// debounce / cooldown / exclusion / stop-suggestion gating — these
// wrappers stay pure-thin.

extension DimmyCore {
    /// Push one mic-activity observation. Returns the rc the Rust core
    /// reported (0 no change, 1 call_detected emitted, 2 call_ended,
    /// 3 meeting.stop_suggested, -1 bad UTF-8). Caller usually ignores
    /// the rc and consumes the events via the existing callback channel.
    @discardableResult
    func callSignalMic(active: Bool, appId: String?) -> Int32 {
        guard isInitialized else { return 0 }
        let mic: Int32 = active ? 1 : 0
        if let appId, !appId.isEmpty {
            return appId.withCString { dimmy_call_signal(mic, $0) }
        }
        return dimmy_call_signal(mic, nil)
    }

    /// Push one system-audio observation (loopback render side). Hosts
    /// that don't poll sys-audio (Mac w/o ScreenCaptureKit loopback)
    /// simply never call this and the stop-suggestion path stays in
    /// mic-only mode.
    @discardableResult
    func callSignalSys(active: Bool, appId: String?) -> Int32 {
        guard isInitialized else { return 0 }
        let sys: Int32 = active ? 1 : 0
        if let appId, !appId.isEmpty {
            return appId.withCString { dimmy_call_signal_sys(sys, $0) }
        }
        return dimmy_call_signal_sys(sys, nil)
    }

    /// Authoritative "the call's process let go of the mic" signal.
    /// Fires `meeting.stop_suggested` immediately (no 5 s silence wait)
    /// when the meeting-origin process disappears from the input set.
    /// Returns the rc (3 = stop_suggested emitted, 0 = no-op). Only
    /// meaningful while a call-detect-driven meeting is active.
    @discardableResult
    func callSignalSessionEnded() -> Int32 {
        guard isInitialized else { return 0 }
        return dimmy_call_signal_session_ended()
    }

    /// Record the user's response. `response` ∈ {"record_now","not_now",
    /// "never","timeout","stop_and_recap","keep_recording","stop_timeout"}.
    /// True iff Rust accepted the response (rc=0).
    @discardableResult
    func callSignalResponse(appId: String?, response: String) -> Bool {
        guard isInitialized else { return false }
        let rc: Int32 = response.withCString { respPtr -> Int32 in
            if let appId, !appId.isEmpty {
                return appId.withCString { dimmy_call_signal_response($0, respPtr) }
            }
            return dimmy_call_signal_response(nil, respPtr)
        }
        if rc != 0 {
            print("[DimmyCore] call_signal_response rc=\(rc) response=\(response)")
        }
        return rc == 0
    }
}

import AppKit
import CoreAudio
import Foundation

/// 4 Hz CoreAudio poll → `dimmy_call_signal` / `dimmy_call_signal_sys` /
/// `dimmy_call_signal_session_ended`. Mirror of `CallDetectionService.cs`
/// on Windows (which fast-polls WASAPI capture sessions).
///
/// Two modes per tick:
///
///   • **Pre-meeting (generic discovery)**: enumerate the processes that
///     currently hold the mic (macOS 14.4+, `kAudioProcessPropertyIs-`
///     `RunningInput`) and treat ANY non-system app that owns input as a
///     call candidate — NOT just a hardcoded VoIP list. So Google Meet /
///     Slack huddle in a browser, Telegram, WhatsApp, Whereby, FaceTime,
///     future voice modes, etc. all trigger a nudge out of the box; the
///     `bundleWhitelist` is now only a cosmetic map to canonical ids for
///     the well-known apps, and `systemBundleIgnore` keeps Apple's mic
///     grabbers (Control Center, Siri) from nudging. On 14.0-14.3 we fall
///     back to device-level `kAudioDevicePropertyDeviceIsRunningSome-`
///     `where` (no attribution, `app=nil`).
///
///   • **Meeting active**: poll amplitude (`dimmy_get_amplitude()` +
///     `dimmy_get_loopback_amplitude()`) against the 0.02 floor so the
///     Rust silence heuristic can still fire `meeting.stop_suggested`.
///     PLUS, when the meeting was started from a detected call (origin
///     pid bound via `markMeetingOrigin()`), watch that process: when it
///     releases the mic, call `dimmy_call_signal_session_ended()` for an
///     immediate, deterministic "call ended?" nudge — no 5 s silence
///     wait. The Rust state machine one-shots stop-suggestion, so the
///     deterministic path and the silence backstop can't double-fire.
///
/// Threading:
///   • Timer fires on `RunLoop.main`.
///   • CoreAudio enumeration (pre-meeting scan + the origin session
///     check) hops to a serial background queue; at most one of each is
///     in flight (overlapping ticks are dropped).
///   • The meeting-amplitude branch stays on main — sub-millisecond FFI.
///
/// Safety hook: never run the *detection* branch while we're recording
/// dictation — cpal opens the mic and CoreAudio reports "input running",
/// which would self-trigger a false `call_detected`. Non-negotiable.
@MainActor
final class CallDetectionManager {
    static let shared = CallDetectionManager()

    private weak var appState: AppState?
    private var pollTimer: Timer?
    private var enabled: Bool = true
    private var lastMicActive: Bool = false
    private var lastSysActive: Bool = false
    private var logSuppress: Int = 0

    /// Background queue for the heavy CoreAudio enumeration. Serial so
    /// at most one scan is in flight at a time.
    private static let scanQueue = DispatchQueue(
        label: "dimmy.calldetect.scan", qos: .utility)
    private var scanInFlight: Bool = false
    private var sessionCheckInFlight: Bool = false

    // Most recent detected candidate from the pre-meeting scan — bound as
    // the meeting origin when the user taps "Record now".
    private var lastCandidatePid: pid_t = 0
    private var lastCandidateApp: String?

    // The process whose call this meeting is recording (0 = manual meeting
    // or pre-14.4, where per-process attribution isn't available).
    private var meetingOriginPid: pid_t = 0
    private var meetingOriginApp: String?
    private var sessionEndedSignaled: Bool = false
    private var originMissingTicks: Int = 0
    private var sessionCheckCounter: Int = 0

    /// Bundle-id prefix → canonical app id for the well-known callers, so
    /// the nudge reads "Microsoft Teams" instead of the raw bundle name.
    /// NOT a gate anymore — discovery is generic; this is cosmetic only.
    private static let bundleWhitelist: [(String, String)] = [
        ("com.microsoft.teams", "teams"),
        ("com.microsoft.teams2", "teams"),
        ("ms-teams", "teams"),
        ("us.zoom.xos", "zoom"),
        ("us.zoom", "zoom"),
        ("com.tinyspeck.slackmacgap", "slack"),
        ("com.slack", "slack"),
        ("com.hnc.discord", "discord"),
        ("com.discord", "discord"),
        ("com.cisco.webexmeetingsapp", "webex"),
        ("com.webex.meetingmanager", "webex"),
        ("com.cisco.webex", "webex"),
    ]

    /// Bundle ids (exact or prefix) that own mic input without being a
    /// real call — Apple's system mic grabbers. Mac equivalent of Windows'
    /// `SystemExesToIgnore`. Everything NOT here is a candidate. Kept
    /// minimal on purpose; the user trains the rest via "Never".
    private static let systemBundleIgnore: [String] = [
        "com.apple.controlcenter",
        "com.apple.siri",
        "com.apple.siriactionsd",
        "com.apple.assistant_service",
    ]

    /// Amplitude floor for "audio is happening" (Mac mirror of Win
    /// `MeetingAmpFloor=0.02f`). ~ -34 dBFS.
    private static let meetingAmpFloor: Float = 0.02

    /// 4 Hz — 250 ms ticks. Mirror of Windows' 250 ms WASAPI poll so the
    /// nudge appears within a beat of joining a call (was 1 Hz = ~1 s lag).
    private static let pollInterval: TimeInterval = 0.25

    private init() {}

    // MARK: - Lifecycle

    func start(appState: AppState) {
        if pollTimer != nil { return }
        self.appState = appState
        let timer = Timer(timeInterval: Self.pollInterval, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.tick() }
        }
        RunLoop.main.add(timer, forMode: .common)
        pollTimer = timer
        print("[CallDetect] started (4 Hz, generic discovery, scan on bg queue)")
    }

    func stop() {
        pollTimer?.invalidate()
        pollTimer = nil
        _ = DimmyCore.shared.callSignalMic(active: false, appId: nil)
    }

    func setEnabled(_ on: Bool) {
        enabled = on
        if !on {
            _ = DimmyCore.shared.callSignalMic(active: false, appId: nil)
            lastMicActive = false
            lastSysActive = false
        }
    }

    /// Bind the just-detected call as this meeting's origin so the
    /// deterministic stop path can watch it. Called from
    /// `AppState.callNudgeRespond` on "record_now". Mirror of Windows'
    /// `MarkMeetingOriginFromCurrentSession()`.
    func markMeetingOrigin() {
        meetingOriginPid = lastCandidatePid
        meetingOriginApp = lastCandidateApp
        sessionEndedSignaled = false
        originMissingTicks = 0
        sessionCheckCounter = 0
        print("[CallDetect] meeting origin bound pid=\(meetingOriginPid) app=\(meetingOriginApp ?? "<none>")")
    }

    private func clearMeetingOrigin() {
        meetingOriginPid = 0
        meetingOriginApp = nil
        sessionEndedSignaled = false
        originMissingTicks = 0
        sessionCheckCounter = 0
    }

    // MARK: - Poll tick (main)

    private func tick() {
        guard enabled else { return }
        let meetingActive = appState?.meetingActive ?? false
        let dictationActive = (appState?.isRecording ?? false) && !meetingActive

        // Safety hook: skip detection while Dimmy opens the mic for
        // dictation — else our own stream self-triggers a call_detected.
        if dictationActive {
            _ = DimmyCore.shared.callSignalMic(active: false, appId: nil)
            return
        }

        if meetingActive {
            // Silence backstop + manual meetings (always).
            meetingAmplitudeTick()
            // Deterministic stop when this meeting came from a detected
            // call: watch the origin process at ~1 Hz (CoreAudio enum is
            // the heavy bit; the nudge doesn't need 4 Hz precision).
            if meetingOriginPid != 0, !sessionEndedSignaled {
                sessionCheckCounter &+= 1
                if sessionCheckCounter >= 4 {
                    sessionCheckCounter = 0
                    sessionEndedCheckTick()
                }
            }
            return
        }

        // Pre-meeting: drop any stale origin from a finished meeting.
        if meetingOriginPid != 0 || meetingOriginApp != nil { clearMeetingOrigin() }

        if scanInFlight { return }
        scanInFlight = true
        Self.scanQueue.async { [weak self] in
            let (micActive, appId, originPid) = Self.scanRunningInputProcesses()
            _ = DimmyCore.shared.callSignalMic(active: micActive, appId: appId)
            Task { @MainActor [weak self] in
                guard let self else { return }
                self.scanInFlight = false
                self.lastCandidatePid = micActive ? originPid : 0
                self.lastCandidateApp = micActive ? appId : nil
                if micActive != self.lastMicActive {
                    print("[CallDetect] mic_active=\(micActive) app=\(appId ?? "<none>") pid=\(originPid)")
                    self.lastMicActive = micActive
                }
            }
        }
    }

    /// Meeting-active branch: amplitude-based mic/sys activity feeding the
    /// Rust silence heuristic. Sub-millisecond FFI — stays on main.
    private func meetingAmplitudeTick() {
        let mic = dimmy_get_amplitude()
        let sys = dimmy_get_loopback_amplitude()
        let micActive = mic > Self.meetingAmpFloor
        let sysActive = sys > Self.meetingAmpFloor

        _ = DimmyCore.shared.callSignalMic(active: micActive, appId: nil)
        _ = DimmyCore.shared.callSignalSys(active: sysActive, appId: nil)

        logSuppress &+= 1
        if logSuppress >= 40 {
            logSuppress = 0
            print(String(
                format: "[CallDetect] meeting-tick mic=%.3f sys=%.3f mic_active=%@ sys_active=%@",
                mic, sys,
                micActive ? "true" : "false",
                sysActive ? "true" : "false"))
        }
        lastMicActive = micActive
        lastSysActive = sysActive
    }

    /// Deterministic stop: is the meeting-origin process still holding the
    /// mic? When it's been gone for ~2 checks (≈2 s), signal session-ended.
    /// Runs the CoreAudio probe off-main; the one-shot guard + Rust state
    /// machine prevent double stop-suggestions vs the silence backstop.
    private func sessionEndedCheckTick() {
        if sessionCheckInFlight { return }
        guard meetingOriginPid != 0, !sessionEndedSignaled else { return }
        let originPid = meetingOriginPid
        sessionCheckInFlight = true
        Self.scanQueue.async { [weak self] in
            var stillRunning = true
            if #available(macOS 14.4, *) {
                stillRunning = Self.inputRunningPids().contains(originPid)
            }
            Task { @MainActor [weak self] in
                guard let self else { return }
                self.sessionCheckInFlight = false
                guard self.meetingOriginPid == originPid, !self.sessionEndedSignaled else { return }
                if stillRunning {
                    self.originMissingTicks = 0
                } else {
                    self.originMissingTicks += 1
                    if self.originMissingTicks >= 2 {
                        self.sessionEndedSignaled = true
                        let rc = DimmyCore.shared.callSignalSessionEnded()
                        print("[CallDetect] origin pid=\(originPid) released mic → session_ended rc=\(rc)")
                    }
                }
            }
        }
    }

    // MARK: - CoreAudio enumeration (callable from any queue)

    /// Returns (any_call_app_active, app_id_or_nil, origin_pid). On 14.4+
    /// the first non-system app holding the mic is the candidate; on
    /// 14.0-14.3 we only know "some input device is running" (app=nil).
    nonisolated private static func scanRunningInputProcesses() -> (Bool, String?, pid_t) {
        if #available(macOS 14.4, *) {
            let selfPid = ProcessInfo.processInfo.processIdentifier
            for pid in inputRunningPids() where pid != selfPid {
                if let appId = resolveAppId(pid) {
                    return (true, appId, pid)
                }
            }
            return (false, nil, 0)
        }
        return (anyInputDeviceRunning(), nil, 0)
    }

    /// Resolve a pid to the canonical/display app id used as the nudge
    /// label + cooldown/exclusion key, or nil if the process is a system
    /// mic grabber / has no app bundle (daemon). Lowercased for stable
    /// keying (the nudge UI Title-cases unknown ids for display).
    nonisolated private static func resolveAppId(_ pid: pid_t) -> String? {
        guard let app = NSRunningApplication(processIdentifier: pid),
              let bundleId = app.bundleIdentifier?.lowercased(), !bundleId.isEmpty
        else { return nil }
        for ignore in systemBundleIgnore {
            let lower = ignore.lowercased()
            if bundleId == lower || bundleId.hasPrefix(lower) { return nil }
        }
        for (prefix, canonical) in bundleWhitelist where bundleId.hasPrefix(prefix.lowercased()) {
            return canonical
        }
        if let name = app.localizedName, !name.isEmpty {
            return name.lowercased()
        }
        return bundleId.split(separator: ".").last.map(String.init)
    }

    /// All pids (incl. system) currently running audio input. macOS 14.4+.
    @available(macOS 14.4, *)
    nonisolated private static func inputRunningPids() -> Set<pid_t> {
        var result = Set<pid_t>()
        var addr = AudioObjectPropertyAddress(
            mSelector: kAudioHardwarePropertyProcessObjectList,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain)
        var size: UInt32 = 0
        let sys = AudioObjectID(kAudioObjectSystemObject)
        if AudioObjectGetPropertyDataSize(sys, &addr, 0, nil, &size) != noErr || size == 0 {
            return result
        }
        let count = Int(size) / MemoryLayout<AudioObjectID>.size
        var ids = [AudioObjectID](repeating: 0, count: count)
        let st = ids.withUnsafeMutableBufferPointer { buf -> OSStatus in
            AudioObjectGetPropertyData(sys, &addr, 0, nil, &size, buf.baseAddress!)
        }
        if st != noErr { return result }

        for procId in ids {
            var inputRunning: UInt32 = 0
            var inputSize = UInt32(MemoryLayout<UInt32>.size)
            var runningAddr = AudioObjectPropertyAddress(
                mSelector: kAudioProcessPropertyIsRunningInput,
                mScope: kAudioObjectPropertyScopeGlobal,
                mElement: kAudioObjectPropertyElementMain)
            if AudioObjectGetPropertyData(procId, &runningAddr, 0, nil, &inputSize, &inputRunning) != noErr {
                continue
            }
            if inputRunning == 0 { continue }

            var pid: pid_t = 0
            var pidSize = UInt32(MemoryLayout<pid_t>.size)
            var pidAddr = AudioObjectPropertyAddress(
                mSelector: kAudioProcessPropertyPID,
                mScope: kAudioObjectPropertyScopeGlobal,
                mElement: kAudioObjectPropertyElementMain)
            if AudioObjectGetPropertyData(procId, &pidAddr, 0, nil, &pidSize, &pid) != noErr {
                continue
            }
            result.insert(pid)
        }
        return result
    }

    /// macOS 14.0-14.3 fallback: per-device "is running somewhere" flag.
    /// No app attribution; true if any input device is captured by anyone
    /// (the dictation-skip hook already excludes our own cpal stream).
    nonisolated private static func anyInputDeviceRunning() -> Bool {
        var devicesSize: UInt32 = 0
        var devicesAddr = AudioObjectPropertyAddress(
            mSelector: kAudioHardwarePropertyDevices,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain)
        let sys = AudioObjectID(kAudioObjectSystemObject)
        if AudioObjectGetPropertyDataSize(sys, &devicesAddr, 0, nil, &devicesSize) != noErr
            || devicesSize == 0 {
            return false
        }
        let count = Int(devicesSize) / MemoryLayout<AudioObjectID>.size
        var devices = [AudioObjectID](repeating: 0, count: count)
        let st = devices.withUnsafeMutableBufferPointer { buf -> OSStatus in
            AudioObjectGetPropertyData(sys, &devicesAddr, 0, nil, &devicesSize, buf.baseAddress!)
        }
        if st != noErr { return false }

        for dev in devices {
            var streamsSize: UInt32 = 0
            var streamsAddr = AudioObjectPropertyAddress(
                mSelector: kAudioDevicePropertyStreams,
                mScope: kAudioDevicePropertyScopeInput,
                mElement: kAudioObjectPropertyElementMain)
            if AudioObjectGetPropertyDataSize(dev, &streamsAddr, 0, nil, &streamsSize) != noErr
                || streamsSize == 0 {
                continue
            }

            var running: UInt32 = 0
            var runSize = UInt32(MemoryLayout<UInt32>.size)
            var runAddr = AudioObjectPropertyAddress(
                mSelector: kAudioDevicePropertyDeviceIsRunningSomewhere,
                mScope: kAudioObjectPropertyScopeGlobal,
                mElement: kAudioObjectPropertyElementMain)
            if AudioObjectGetPropertyData(dev, &runAddr, 0, nil, &runSize, &running) != noErr {
                continue
            }
            if running != 0 { return true }
        }
        return false
    }
}

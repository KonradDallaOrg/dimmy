import AppKit
import CoreAudio
import Foundation

/// 1 Hz CoreAudio poll → `dimmy_call_signal` / `dimmy_call_signal_sys`.
/// Mirror of `Services/CallDetectionService.cs` on Windows.
///
/// Two modes per tick:
///
///   • **Pre-meeting**: enumerate running input processes (macOS 14.4+,
///     `kAudioProcessPropertyIsRunningInput`) and match bundle ids
///     against the VoIP whitelist (teams/zoom/slack/discord/webex). On
///     14.0-14.3 fall back to `kAudioDevicePropertyDeviceIsRunningSome-`
///     `where` on each input device — no per-process attribution, just
///     mic-active=true with `app=nil` ("Microphone in use" headline).
///
///   • **Meeting active**: pivot to amplitude. Poll
///     `dimmy_get_amplitude()` + `dimmy_get_loopback_amplitude()` against
///     the same 0.02 floor Windows uses. The Rust state machine
///     emits `meeting.stop_suggested` when BOTH have been silent past
///     their respective thresholds.
///
/// Threading:
///   • Timer fires on `RunLoop.main` so the safety-state read is
///     trivially actor-isolated (no lock contention with AppState).
///   • Pre-meeting enumeration is the slow path (CoreAudio property
///     fetches × N processes + `NSRunningApplication` lookups), so it
///     hops to a serial background queue. The pill animation runs on
///     main — keeping the scan there for ~100 ms per tick was visibly
///     hitchy on busy hosts.
///   • The dictation-skip and meeting-amplitude branches stay on main
///     — they're sub-millisecond FFI calls and the responsiveness of
///     stop-suggestion benefits from minimal latency.
///
/// Safety hook: never tick the *detection* branch while we're recording
/// dictation — cpal opens the mic and CoreAudio reports "input running",
/// which would trigger a false `call_detected` mid-dictation. Burned on
/// Windows before (`CallDetectionService.cs:126`). Non-negotiable.
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
    /// at most one scan is in flight at a time — if main schedules a
    /// new tick while the previous is still running we just drop it.
    private static let scanQueue = DispatchQueue(
        label: "dimmy.calldetect.scan", qos: .utility)
    private var scanInFlight: Bool = false

    /// Bundle-id prefix → canonical app id sent to Rust. Keep lowercase.
    /// Mirror of Windows ProcessToAppId in CallDetectionService.cs:49-66.
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

    /// Amplitude floor for "audio is happening" (Mac mirror of
    /// Win `MeetingAmpFloor=0.02f`). ~ -34 dBFS — kills HVAC hum
    /// without losing soft conversation.
    private static let meetingAmpFloor: Float = 0.02

    private init() {}

    // MARK: - Lifecycle

    func start(appState: AppState) {
        if pollTimer != nil { return }
        self.appState = appState
        let timer = Timer(timeInterval: 1.0, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.tick() }
        }
        RunLoop.main.add(timer, forMode: .common)
        pollTimer = timer
        print("[CallDetect] started (1 Hz, scan on bg queue)")
    }

    func stop() {
        pollTimer?.invalidate()
        pollTimer = nil
        // Tell Rust we're not signalling anymore so cooldown timers can
        // wind down cleanly.
        _ = DimmyCore.shared.callSignalMic(active: false, appId: nil)
    }

    func setEnabled(_ on: Bool) {
        enabled = on
        if !on {
            // Mirror Win: zero out the state if user disables mid-flight.
            _ = DimmyCore.shared.callSignalMic(active: false, appId: nil)
            lastMicActive = false
            lastSysActive = false
        }
    }

    // MARK: - Poll tick (main)

    private func tick() {
        guard enabled else { return }
        let meetingActive = appState?.meetingActive ?? false
        let dictationActive = (appState?.isRecording ?? false) && !meetingActive

        // Safety hook: skip the entire tick while Dimmy itself opens
        // the mic for dictation. Without this, the pill shortcut would
        // trigger a `call_detected` 5 s in. CallDetectionService.cs:126.
        if dictationActive {
            _ = DimmyCore.shared.callSignalMic(active: false, appId: nil)
            return
        }

        if meetingActive {
            meetingAmplitudeTick()
            return
        }

        // Pre-meeting branch — enumeration goes off-main.
        if scanInFlight { return }
        scanInFlight = true
        Self.scanQueue.async { [weak self] in
            let (micActive, appId) = Self.scanRunningInputProcesses()
            _ = DimmyCore.shared.callSignalMic(active: micActive, appId: appId)
            Task { @MainActor [weak self] in
                guard let self else { return }
                self.scanInFlight = false
                if micActive != self.lastMicActive {
                    print("[CallDetect] mic_active=\(micActive) app=\(appId ?? "<none>")")
                    self.lastMicActive = micActive
                }
            }
        }
    }

    /// Meeting-active branch: amplitude-based mic/sys activity. Both
    /// FFI calls are sub-millisecond — stays on main.
    private func meetingAmplitudeTick() {
        let mic = dimmy_get_amplitude()
        let sys = dimmy_get_loopback_amplitude()
        let micActive = mic > Self.meetingAmpFloor
        let sysActive = sys > Self.meetingAmpFloor

        _ = DimmyCore.shared.callSignalMic(active: micActive, appId: nil)
        _ = DimmyCore.shared.callSignalSys(active: sysActive, appId: nil)

        logSuppress &+= 1
        if logSuppress >= 15 {
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

    // MARK: - CoreAudio enumeration (callable from any queue)

    /// Returns (any_mic_active, whitelist_app_id_or_nil). Uses the
    /// macOS 14.4+ per-process API when available, else falls back to
    /// device-level "running somewhere" with `app=nil`.
    nonisolated private static func scanRunningInputProcesses() -> (Bool, String?) {
        if #available(macOS 14.4, *) {
            return scanRunningInputProcesses_v14_4()
        }
        return (anyInputDeviceRunning(), nil)
    }

    @available(macOS 14.4, *)
    nonisolated private static func scanRunningInputProcesses_v14_4() -> (Bool, String?) {
        var addr = AudioObjectPropertyAddress(
            mSelector: kAudioHardwarePropertyProcessObjectList,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain)
        var size: UInt32 = 0
        let sys = AudioObjectID(kAudioObjectSystemObject)
        if AudioObjectGetPropertyDataSize(sys, &addr, 0, nil, &size) != noErr || size == 0 {
            return (anyInputDeviceRunning(), nil)
        }
        let count = Int(size) / MemoryLayout<AudioObjectID>.size
        var ids = [AudioObjectID](repeating: 0, count: count)
        let st = ids.withUnsafeMutableBufferPointer { buf -> OSStatus in
            AudioObjectGetPropertyData(sys, &addr, 0, nil, &size, buf.baseAddress!)
        }
        if st != noErr {
            return (anyInputDeviceRunning(), nil)
        }

        var anyActive = false
        var inferred: String?
        let selfPid = ProcessInfo.processInfo.processIdentifier

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
            if pid == selfPid { continue }

            anyActive = true
            if inferred == nil,
               let bundleId = NSRunningApplication(processIdentifier: pid)?
                .bundleIdentifier?.lowercased() {
                for (prefix, canonical) in bundleWhitelist {
                    if bundleId.hasPrefix(prefix.lowercased()) {
                        inferred = canonical
                        break
                    }
                }
            }
        }
        return (anyActive, inferred)
    }

    /// macOS 14.0-14.3 fallback: per-device "is running somewhere"
    /// flag. No app attribution; returns true if any input device is
    /// captured by *anyone* on the system (caller already filtered out
    /// our own cpal stream via the safety hook).
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

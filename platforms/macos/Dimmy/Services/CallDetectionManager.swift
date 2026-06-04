import AppKit
import Combine
import CoreAudio
import Foundation

/// Event-driven CoreAudio call-detect → `dimmy_call_signal` /
/// `dimmy_call_signal_sys` / `dimmy_call_signal_session_ended`. Windows
/// equivalent is `CallDetectionService.cs` (which polls WASAPI because
/// session events aren't reliable there).
///
/// Three jobs, each with its own activation pattern:
///
///   • **Pre-meeting scan (Job 1, event-driven)**: discover which app
///     is holding the mic OR producing whitelisted audio output, push
///     a `call_signal(active, app)` to Rust → state machine emits the
///     "Record now" nudge. Fires on:
///     - `kAudioObjectSystemObject` /
///       `kAudioHardwarePropertyProcessObjectList` (any process opens
///       its first audio stream or closes its last one), and
///     - per-process `kAudioProcessPropertyIsRunningInput` and
///       `kAudioProcessPropertyIsRunning` (mic / output state change
///       per process).
///     Plus `appState.$recordingState` (dictation start/stop, so we
///     suppress detection when Dimmy itself opens the mic) and
///     `appState.$meetingActive` (transitions). 30 s `DispatchSourceTimer`
///     backstop as safety net for missed HAL events.
///
///     Input side is generic discovery (any non-system app holding mic);
///     output side is gated to the curated `bundleWhitelist` (Zoom, Teams,
///     Meet/Slack, Discord, Webex) so Spotify / YouTube don't spuriously
///     nudge. `systemBundleIgnore` keeps Apple's mic grabbers (Control
///     Center, Siri) out of both sides.
///
///   • **Meeting-active origin presence (Job 2, event-driven)**: when a
///     meeting is bound to an origin PID (via "Record now" or mid-meeting
///     adoption), watch that PID's mic+output flags. When BOTH go away
///     (call closed), fire `signal_session_ended` for an immediate stop-
///     suggestion. Same listeners as Job 1.
///
///   • **Meeting-active amplitude streaming (Job 3, MUST stay polling)**:
///     read `dimmy_get_amplitude()` + `dimmy_get_loopback_amplitude()`
///     at 4 Hz, push to the Rust silence-heuristic gate. There's no
///     "amplitude changed" event on Core Audio — this is a streaming
///     signal, not a state-change event. The 250 ms `Timer` is armed
///     only while `meetingActive == true` and torn down on stop;
///     pre-meeting and dictation periods have no polling at all.
///
/// macOS 14.0-14.3 fallback: HAL per-process properties exist only on
/// 14.4+. On older OSes we lose attribution but keep the device-level
/// `kAudioDevicePropertyDeviceIsRunningSomewhere` poll (still triggered
/// by HAL events when available, by the 30 s backstop otherwise).
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

    /// 250 ms timer for Job 3 — armed only while `meetingActive` is true.
    /// Pre-meeting / dictation periods have no polling at all.
    private var amplitudeTimer: Timer?

    /// 30 s safety backstop running on `listenerQueue` — re-fires the scan
    /// handler in case a HAL property listener missed an event. Idempotent
    /// with the listener path (same `callSignal` call). Cost: ~2 ms / 30 s.
    private var scanBackstop: DispatchSourceTimer?

    /// HAL property listener state for Jobs 1+2. Shape mirrors the
    /// SystemAudioProcessTap rescan listeners (event-driven primary path,
    /// `NSLock`-protected dictionaries to allow main↔listenerQueue access).
    private var processListListener: AudioObjectPropertyListenerBlock?
    private var perProcessInputListeners: [AudioObjectID: AudioObjectPropertyListenerBlock] = [:]
    private var perProcessOutputListeners: [AudioObjectID: AudioObjectPropertyListenerBlock] = [:]
    private let listenerLock = NSLock()
    private static let listenerQueue = DispatchQueue(
        label: "dimmy.calldetect.listeners", qos: .utility)

    /// Combine sinks on `$meetingActive` / `$recordingState` drive the
    /// amplitude-timer arm/disarm + immediate scan re-evaluation on
    /// dictation start/stop. Cleared on `stop()` to avoid retain cycles.
    private var cancellables = Set<AnyCancellable>()

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
    private var adoptInFlight: Bool = false

    // Most recent detected candidate from the pre-meeting scan — bound as
    // the meeting origin when the user taps "Record now".
    private var lastCandidatePid: pid_t = 0
    private var lastCandidateApp: String?

    // The process whose call this meeting is recording (0 = manual meeting
    // or pre-14.4, where per-process attribution isn't available).
    private var meetingOriginPid: pid_t = 0
    private var meetingOriginApp: String?
    private var sessionEndedSignaled: Bool = false

    /// One-shot 2 s debounce that replaces the polling-era
    /// `originMissingTicks >= 2` threshold. Scheduled the first time the
    /// origin PID is found gone; if it comes back before the timer fires
    /// the timer is canceled. If the timer fires and the origin is still
    /// gone, `signal_session_ended` is pushed to Rust.
    private var sessionEndedConfirmTimer: DispatchSourceTimer?

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
        guard scanBackstop == nil else { return }
        self.appState = appState

        // Combine subscriptions drive state transitions that the old
        // 250 ms tick used to discover via polling:
        //   - meetingActive edges arm / disarm the amplitude timer + clear
        //     the origin tracker; re-run the scan handler on either edge.
        //   - recordingState edges trigger the scan handler so the
        //     dictation-active gate (suppresses self-detection while Dimmy
        //     opens the mic) reacts immediately instead of within ≤250 ms.
        appState.$meetingActive
            .removeDuplicates()
            .sink { [weak self] active in
                Task { @MainActor [weak self] in
                    guard let self else { return }
                    if active {
                        self.startAmplitudeTimer()
                    } else {
                        self.stopAmplitudeTimer()
                        self.clearMeetingOrigin()
                    }
                    self.handleScanEvent()
                }
            }
            .store(in: &cancellables)
        appState.$recordingState
            .removeDuplicates()
            .sink { [weak self] _ in
                Task { @MainActor in self?.handleScanEvent() }
            }
            .store(in: &cancellables)

        // HAL property listeners — Job 1 + Job 2 primary path.
        if #available(macOS 14.4, *) {
            startEventListeners()
        }

        // 30 s safety backstop on listenerQueue. Idempotent with the
        // listener path: same handleScanEvent body, the scan-inflight
        // guard collapses concurrent fires into one HAL sweep.
        let backstop = DispatchSource.makeTimerSource(queue: Self.listenerQueue)
        backstop.schedule(deadline: .now() + 30.0, repeating: 30.0)
        backstop.setEventHandler { [weak self] in
            Task { @MainActor in self?.handleScanEvent() }
        }
        backstop.resume()
        scanBackstop = backstop

        // Bootstrap: if a call app is already running when CallDetect
        // starts (e.g. Dimmy launched into an in-progress Zoom call),
        // surface it now without waiting for the next HAL transition.
        handleScanEvent()

        if appState.meetingActive {
            startAmplitudeTimer()
        }

        print("[CallDetect] started (event-driven HAL listeners + 30 s backstop, amplitude timer on meeting)")
    }

    func stop() {
        stopAmplitudeTimer()
        scanBackstop?.cancel()
        scanBackstop = nil
        if #available(macOS 14.4, *) {
            stopEventListeners()
        }
        cancellables.removeAll()
        _ = DimmyCore.shared.callSignalMic(active: false, appId: nil)
    }

    private func startAmplitudeTimer() {
        guard amplitudeTimer == nil else { return }
        let timer = Timer(timeInterval: Self.pollInterval, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.amplitudeTick() }
        }
        RunLoop.main.add(timer, forMode: .common)
        amplitudeTimer = timer
    }

    private func stopAmplitudeTimer() {
        amplitudeTimer?.invalidate()
        amplitudeTimer = nil
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
        sessionEndedConfirmTimer?.cancel()
        sessionEndedConfirmTimer = nil
        // Suppress the Rust silence backstop iff we actually bound a process to
        // watch — otherwise (no candidate pid) keep the backstop as the only
        // stop signal. Watching a real pid is the deterministic authority.
        _ = dimmy_call_set_tracked_origin(meetingOriginPid != 0 ? 1 : 0)
        print("[CallDetect] meeting origin bound pid=\(meetingOriginPid) app=\(meetingOriginApp ?? "<none>")")
    }

    private func clearMeetingOrigin() {
        meetingOriginPid = 0
        meetingOriginApp = nil
        sessionEndedSignaled = false
        sessionEndedConfirmTimer?.cancel()
        sessionEndedConfirmTimer = nil
        // No tracked process → re-enable the silence backstop.
        _ = dimmy_call_set_tracked_origin(0)
    }

    // MARK: - Event handlers

    /// Job 1 + Job 2 scan dispatch. Fires from any of:
    /// - System / per-process HAL property listeners (the primary path,
    ///   latency ~5-20 ms);
    /// - `appState.$meetingActive` / `$recordingState` Combine sinks
    ///   (transitions);
    /// - 30 s `scanBackstop` (safety net for missed events);
    /// - `start()` bootstrap.
    ///
    /// Idempotent: the `scanInFlight` guard collapses concurrent fires
    /// into a single HAL sweep; the downstream Rust state machine
    /// `last_mic_active` debounce makes repeated identical signals a no-op.
    private func handleScanEvent() {
        guard enabled else { return }
        let meetingActive = appState?.meetingActive ?? false
        let dictationActive = (appState?.isRecording ?? false) && !meetingActive

        // Dictation gate: Dimmy's own cpal mic stream would self-trigger
        // call_detected otherwise. Force-clear the mic-side signal so
        // any pending nudge state collapses to "no call".
        if dictationActive {
            _ = DimmyCore.shared.callSignalMic(active: false, appId: nil)
            return
        }

        if meetingActive {
            // Meeting-active scan-side work (Job 2): origin presence check
            // (or adoption when no origin is bound yet). Note: amplitude
            // streaming (Job 3) runs on the separate 250 ms amplitudeTimer.
            guard !sessionEndedSignaled else { return }
            if meetingOriginPid == 0 {
                adoptCallOriginDuringMeetingTick()
            } else {
                sessionEndedCheckTick()
            }
            return
        }

        // Pre-meeting scan (Job 1).
        if scanInFlight { return }
        scanInFlight = true
        Self.scanQueue.async { [weak self] in
            let (micActive, appId, originPid) = Self.scanRunningProcesses()
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

    /// Job 3 — amplitude-based mic/sys activity feeding the Rust silence
    /// heuristic. Fires from the 250 ms `amplitudeTimer`, which is alive
    /// only while `meetingActive == true`. Sub-millisecond FFI — stays on
    /// main. NOT event-replaceable: amplitude is a streaming signal, not
    /// a state-change event, and the Rust heuristic needs continuous
    /// feeds to gate the ~5 s silence threshold.
    private func amplitudeTick() {
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

    /// Deterministic stop: is the meeting-origin process still alive on
    /// EITHER side (mic input or audio output)? When it's been gone from
    /// both for ~2 s (debounced via one-shot `sessionEndedConfirmTimer`),
    /// signal session-ended. The one-shot guard + Rust state machine
    /// prevent double stop-suggestions vs the silence backstop.
    ///
    /// Event-driven: this runs every time a HAL listener fires (the
    /// origin PID's mic-running or output-running flag flipped, or any
    /// process appeared/disappeared from the system process list). The
    /// 2 s debounce protects against transient device-handoff releases
    /// (BT pairing switch, sample-rate renegotiation) where Zoom's mic
    /// is briefly relinquished but recaptured almost immediately.
    ///
    /// Checking both sides matters because the origin may have been
    /// bound via output-side detection ("joined Zoom muted" case). Such a
    /// PID never appears in `inputRunningPids` even while the call is
    /// alive — watching only input would fire a false session_ended the
    /// first time the listener fires after origin bind.
    private func sessionEndedCheckTick() {
        if sessionCheckInFlight { return }
        guard meetingOriginPid != 0, !sessionEndedSignaled else { return }
        let originPid = meetingOriginPid
        sessionCheckInFlight = true
        Self.scanQueue.async { [weak self] in
            var stillRunning = true
            if #available(macOS 14.4, *) {
                stillRunning = Self.inputRunningPids().contains(originPid)
                    || SystemAudioProcessTap.outputRunningPids().contains(originPid)
            }
            Task { @MainActor [weak self] in
                guard let self else { return }
                self.sessionCheckInFlight = false
                guard self.meetingOriginPid == originPid, !self.sessionEndedSignaled else { return }
                if stillRunning {
                    // Origin alive — cancel any pending confirmation.
                    self.sessionEndedConfirmTimer?.cancel()
                    self.sessionEndedConfirmTimer = nil
                    return
                }
                // Origin gone. If we already have a pending 2 s
                // confirmation timer, let it fire — repeated listener
                // events confirming "still gone" don't restart the clock.
                guard self.sessionEndedConfirmTimer == nil else { return }
                let timer = DispatchSource.makeTimerSource(queue: .main)
                timer.schedule(deadline: .now() + 2.0)
                timer.setEventHandler { [weak self] in
                    Task { @MainActor [weak self] in
                        guard let self else { return }
                        guard self.meetingOriginPid == originPid,
                              !self.sessionEndedSignaled else {
                            self.sessionEndedConfirmTimer = nil
                            return
                        }
                        var stillGone = true
                        if #available(macOS 14.4, *) {
                            stillGone = !Self.inputRunningPids().contains(originPid)
                                && !SystemAudioProcessTap.outputRunningPids().contains(originPid)
                        }
                        self.sessionEndedConfirmTimer = nil
                        if stillGone {
                            self.sessionEndedSignaled = true
                            let rc = DimmyCore.shared.callSignalSessionEnded()
                            print("[CallDetect] origin pid=\(originPid) released mic+output → session_ended rc=\(rc)")
                        }
                    }
                }
                timer.resume()
                self.sessionEndedConfirmTimer = timer
            }
        }
    }

    /// Adopt a call as the meeting origin WHILE a meeting is already
    /// active — for a manually-started meeting (pill / window) where no
    /// "Record now" nudge ran so `markMeetingOrigin()` was never called.
    /// Reuses the same `scanRunningProcesses()` discovery as the
    /// pre-meeting tick (excludes self pid + system bundles). On adoption
    /// arms the Rust state machine via `callMeetingStartedExternal()` so
    /// `signal_session_ended` will fire when the call closes — without
    /// arming, `recording_active_from_us` stays false and the
    /// stop-suggestion path is a silent NoChange. Mirror of Windows
    /// `TryAdoptCallOriginDuringMeeting`.
    private func adoptCallOriginDuringMeetingTick() {
        if adoptInFlight { return }
        guard meetingOriginPid == 0, !sessionEndedSignaled else { return }
        adoptInFlight = true
        Self.scanQueue.async { [weak self] in
            let (active, appId, originPid) = Self.scanRunningProcesses()
            Task { @MainActor [weak self] in
                guard let self else { return }
                self.adoptInFlight = false
                // The meeting could have ended, or another path bound the
                // origin (e.g. markMeetingOrigin) while the scan ran.
                guard self.appState?.meetingActive == true,
                      self.meetingOriginPid == 0,
                      !self.sessionEndedSignaled
                else { return }
                if active, originPid != 0, let appId {
                    self.meetingOriginPid = originPid
                    self.meetingOriginApp = appId
                    self.sessionEndedConfirmTimer?.cancel()
                    self.sessionEndedConfirmTimer = nil
                    _ = DimmyCore.shared.callMeetingStartedExternal()
                    // Now watching a real process deterministically → suppress
                    // the Rust silence backstop (the 15-popups bug).
                    _ = dimmy_call_set_tracked_origin(1)
                    print("[CallDetect] adopted call origin mid-meeting pid=\(originPid) app=\(appId)")
                }
            }
        }
    }

    // MARK: - HAL event listeners (macOS 14.4+)

    /// Register property listeners that drive Jobs 1 + 2 in real time:
    ///   - `kAudioHardwarePropertyProcessObjectList` on the system object
    ///     (any process registers or de-registers with coreaudiod);
    ///   - `kAudioProcessPropertyIsRunningInput` per process (mic capture
    ///     transition);
    ///   - `kAudioProcessPropertyIsRunning` per process (audio output
    ///     transition).
    ///
    /// All callbacks fan into `handleScanEvent()` on main. Idempotent at
    /// every layer: the in-flight guards in `handleScanEvent` /
    /// `sessionEndedCheckTick` / `adoptCallOriginDuringMeetingTick`
    /// collapse concurrent HAL events into a single check.
    @available(macOS 14.4, *)
    private func startEventListeners() {
        listenerLock.lock()
        let alreadyArmed = processListListener != nil
        listenerLock.unlock()
        if alreadyArmed { return }

        var sysAddr = AudioObjectPropertyAddress(
            mSelector: kAudioHardwarePropertyProcessObjectList,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain)
        let sysBlock: AudioObjectPropertyListenerBlock = { [weak self] _, _ in
            guard let self else { return }
            self.refreshPerProcessListeners()
            Task { @MainActor in self.handleScanEvent() }
        }
        let st = AudioObjectAddPropertyListenerBlock(
            AudioObjectID(kAudioObjectSystemObject), &sysAddr, Self.listenerQueue, sysBlock)
        if st == noErr {
            listenerLock.lock()
            processListListener = sysBlock
            listenerLock.unlock()
            print("[CallDetect] event listener armed on ProcessObjectList")
        } else {
            print("[CallDetect] AddPropertyListener(ProcessObjectList) failed: \(st) — backstop only")
        }

        refreshPerProcessListeners()
    }

    @available(macOS 14.4, *)
    private func stopEventListeners() {
        listenerLock.lock()
        let oldSys = processListListener
        let oldInputs = perProcessInputListeners
        let oldOutputs = perProcessOutputListeners
        processListListener = nil
        perProcessInputListeners.removeAll()
        perProcessOutputListeners.removeAll()
        listenerLock.unlock()

        if let block = oldSys {
            var sysAddr = AudioObjectPropertyAddress(
                mSelector: kAudioHardwarePropertyProcessObjectList,
                mScope: kAudioObjectPropertyScopeGlobal,
                mElement: kAudioObjectPropertyElementMain)
            _ = AudioObjectRemovePropertyListenerBlock(
                AudioObjectID(kAudioObjectSystemObject), &sysAddr, Self.listenerQueue, block)
        }
        for (obj, block) in oldInputs {
            var addr = AudioObjectPropertyAddress(
                mSelector: kAudioProcessPropertyIsRunningInput,
                mScope: kAudioObjectPropertyScopeGlobal,
                mElement: kAudioObjectPropertyElementMain)
            _ = AudioObjectRemovePropertyListenerBlock(obj, &addr, Self.listenerQueue, block)
        }
        for (obj, block) in oldOutputs {
            var addr = AudioObjectPropertyAddress(
                mSelector: kAudioProcessPropertyIsRunning,
                mScope: kAudioObjectPropertyScopeGlobal,
                mElement: kAudioObjectPropertyElementMain)
            _ = AudioObjectRemovePropertyListenerBlock(obj, &addr, Self.listenerQueue, block)
        }
    }

    /// Subscribe to mic-input and audio-output property changes for every
    /// audio process the HAL knows about (minus our own pid). Idempotent;
    /// unsubscribes from vanished objects + subscribes to new ones. Called
    /// from `startEventListeners` on initial setup and from the system
    /// listener whenever the process list changes.
    @available(macOS 14.4, *)
    private func refreshPerProcessListeners() {
        let allObjects = Set(SystemAudioProcessTap.allAudioProcessObjects())
        let selfPid = ProcessInfo.processInfo.processIdentifier

        listenerLock.lock()
        // Drop listeners for vanished processes.
        let goneInputs = perProcessInputListeners.keys.filter { !allObjects.contains($0) }
        for obj in goneInputs {
            if let block = perProcessInputListeners[obj] {
                var addr = AudioObjectPropertyAddress(
                    mSelector: kAudioProcessPropertyIsRunningInput,
                    mScope: kAudioObjectPropertyScopeGlobal,
                    mElement: kAudioObjectPropertyElementMain)
                _ = AudioObjectRemovePropertyListenerBlock(obj, &addr, Self.listenerQueue, block)
            }
            perProcessInputListeners.removeValue(forKey: obj)
        }
        let goneOutputs = perProcessOutputListeners.keys.filter { !allObjects.contains($0) }
        for obj in goneOutputs {
            if let block = perProcessOutputListeners[obj] {
                var addr = AudioObjectPropertyAddress(
                    mSelector: kAudioProcessPropertyIsRunning,
                    mScope: kAudioObjectPropertyScopeGlobal,
                    mElement: kAudioObjectPropertyElementMain)
                _ = AudioObjectRemovePropertyListenerBlock(obj, &addr, Self.listenerQueue, block)
            }
            perProcessOutputListeners.removeValue(forKey: obj)
        }

        // Subscribe to new ones (skip self — Dimmy's own cpal mic
        // stream during dictation would self-trigger the scan).
        for obj in allObjects {
            if SystemAudioProcessTap.pid(forAudioObject: obj) == selfPid { continue }

            if perProcessInputListeners[obj] == nil {
                var addr = AudioObjectPropertyAddress(
                    mSelector: kAudioProcessPropertyIsRunningInput,
                    mScope: kAudioObjectPropertyScopeGlobal,
                    mElement: kAudioObjectPropertyElementMain)
                let block: AudioObjectPropertyListenerBlock = { [weak self] _, _ in
                    Task { @MainActor in self?.handleScanEvent() }
                }
                if AudioObjectAddPropertyListenerBlock(obj, &addr, Self.listenerQueue, block) == noErr {
                    perProcessInputListeners[obj] = block
                }
            }

            if perProcessOutputListeners[obj] == nil {
                var addr = AudioObjectPropertyAddress(
                    mSelector: kAudioProcessPropertyIsRunning,
                    mScope: kAudioObjectPropertyScopeGlobal,
                    mElement: kAudioObjectPropertyElementMain)
                let block: AudioObjectPropertyListenerBlock = { [weak self] _, _ in
                    Task { @MainActor in self?.handleScanEvent() }
                }
                if AudioObjectAddPropertyListenerBlock(obj, &addr, Self.listenerQueue, block) == noErr {
                    perProcessOutputListeners[obj] = block
                }
            }
        }
        listenerLock.unlock()
    }

    // MARK: - CoreAudio enumeration (callable from any queue)

    /// Returns (any_call_app_active, app_id_or_nil, origin_pid). On 14.4+
    /// the first non-system app holding the mic is the candidate; if none,
    /// the first known-call-app producing audio output wins (catches the
    /// "joined Zoom muted" case where the user's mic isn't open but the
    /// peer's audio is flowing). On 14.0-14.3 we only know "some input
    /// device is running" (app=nil).
    ///
    /// Input over output is deliberate: input is the stronger signal
    /// (recording a user's mic is unambiguously a call action), output
    /// only fires for a curated bundleWhitelist set to avoid Spotify /
    /// YouTube nudges. Tested in `CallDetectionCandidateSelectionTests`
    /// — the gated resolver returns nil for non-whitelist bundles so the
    /// `firstCallCandidate` picker skips them.
    nonisolated private static func scanRunningProcesses() -> (Bool, String?, pid_t) {
        if #available(macOS 14.4, *) {
            let selfPid = ProcessInfo.processInfo.processIdentifier
            // Input side: generic discovery.
            let inputPids = inputRunningPids()
            if let (pid, appId) = firstCallCandidate(
                pids: inputPids, selfPid: selfPid, resolve: resolveAppId
            ) {
                return (true, appId, pid)
            }
            // Output side: known-call-app gated. Reuses the shared HAL
            // enumeration from SystemAudioProcessTap so call-detect and
            // per-process tap input selection see the same world.
            let outputPids = SystemAudioProcessTap.outputRunningPids()
            if let (pid, appId) = firstCallCandidate(
                pids: outputPids, selfPid: selfPid, resolve: resolveKnownCallApp
            ) {
                return (true, appId, pid)
            }
            return (false, nil, 0)
        }
        return (anyInputDeviceRunning(), nil, 0)
    }

    /// Pure: pick the first non-self pid whose resolver returns a non-nil
    /// app id. Deterministic — sorts pids ascending so production and
    /// tests agree on which origin wins when multiple call apps hold the
    /// mic simultaneously. Returns nil when no real call app is present
    /// (manual meeting with no call → adoption skips, behaviour unchanged
    /// from the pre-feature baseline). Internal access so DimmyTests can
    /// pin the candidate-selection logic without going through CoreAudio.
    nonisolated static func firstCallCandidate(
        pids: Set<pid_t>, selfPid: pid_t, resolve: (pid_t) -> String?
    ) -> (pid_t, String)? {
        for pid in pids.sorted() where pid != selfPid {
            if let appId = resolve(pid) {
                return (pid, appId)
            }
        }
        return nil
    }

    /// Resolve a pid to the canonical/display app id used as the nudge
    /// label + cooldown/exclusion key, or nil if the process is a system
    /// mic grabber / has no app bundle (daemon). Lowercased for stable
    /// keying (the nudge UI Title-cases unknown ids for display).
    nonisolated static func resolveAppId(_ pid: pid_t) -> String? {
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

    /// Strict variant of `resolveAppId`: returns the canonical id ONLY
    /// when the bundle matches the curated `bundleWhitelist` (Zoom,
    /// Teams, Meet/Slack, Discord, Webex, …). Used by the OUTPUT-side
    /// branch of `scanRunningProcesses`: generic discovery on output
    /// would nudge for Spotify / YouTube / Music, which is a UX bug.
    /// The whitelist is the same cosmetic-mapping table the input-side
    /// uses for display labels — single source of truth for "is this a
    /// known meeting app".
    nonisolated static func resolveKnownCallApp(_ pid: pid_t) -> String? {
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
        return nil
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

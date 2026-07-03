import AppKit
import ApplicationServices
import Carbon
import Combine
import CoreGraphics
import Darwin

/// Write a line to /tmp/dimmy-hotkey.log with immediate flush — survives any buffering quirks.
func hkLog(_ msg: String) {
    NSLog("%@", msg)
    let line = "\(Date()): \(msg)\n"
    guard let data = line.data(using: .utf8) else { return }
    let path = "/tmp/dimmy-hotkey.log"
    let fd = open(path, O_WRONLY | O_CREAT | O_APPEND, 0o644)
    if fd >= 0 {
        _ = data.withUnsafeBytes { write(fd, $0.baseAddress, data.count) }
        fsync(fd)
        close(fd)
    }
}

extension NSEvent.ModifierFlags {
    init(cgFlags: CGEventFlags) {
        var f: NSEvent.ModifierFlags = []
        if cgFlags.contains(.maskCommand) { f.insert(.command) }
        if cgFlags.contains(.maskAlternate) { f.insert(.option) }
        if cgFlags.contains(.maskShift) { f.insert(.shift) }
        if cgFlags.contains(.maskControl) { f.insert(.control) }
        if cgFlags.contains(.maskSecondaryFn) { f.insert(.function) }
        self = f
    }
}

@MainActor
final class HotkeyManager {
    static let shared = HotkeyManager()

    // Active global interception (consumes events from other apps). Requires Accessibility.
    private var eventTap: CFMachPort?
    private var runLoopSource: CFRunLoopSource?

    // Polling timer that tries to install the tap once Accessibility is granted.
    private var accessibilityPollTimer: Timer?

    // Re-install the tap after sleep/wake (macOS disables taps during sleep).
    private var wakeObserver: NSObjectProtocol?

    // Track modifier state
    private var controlOptionDown = false

    // Double-tap detection for toggle mode
    private var lastReleaseTime: Date?
    private var lastPressTime: Date?
    private let doubleTapInterval: TimeInterval = 0.4

    // Minimum hold to avoid accidental triggers
    private let minimumHoldDuration: TimeInterval = 0.15

    // Amplitude polling timer for waveform animation
    private var amplitudeTimer: Timer?

    private var appState: AppState?

    // Dedicated Command-Mode hotkey runs on the SAME CGEventTap as the
    // dictation modifier-only shortcut — the tap mask now covers
    // .flagsChanged + .keyDown + .keyUp so a single hook sees both
    // modifier-only chords (e.g. ⌃⌥) AND modifier+key chords (e.g.
    // ⌃⇧X) AND the matching releases. The pattern mirrors the Win-side
    // refactor that moved the command hotkey off `RegisterHotKey` (which
    // is key-down-only, can't do PTT, can't do modifier-only) onto the
    // Rust low-level keyboard hook. Carbon `RegisterEventHotKey` had the
    // same limitations on Mac; this replaces it with a Swift state
    // machine that honours `appState.preferredMode` (toggle vs PTT).
    private let commandComboState = CommandComboState()
    private var commandLastPressTime: Date?

    // Dedicated meeting start/stop hotkey — same CGEventTap + combo matcher as
    // the command hotkey, but TOGGLE-only (a meeting can't be push-to-talk):
    // we act on the pressed edge and ignore the release.
    private let meetingComboState = CommandComboState()

    private init() {
        hkLog("[HotkeyManager] singleton init")
    }

    func start(appState: AppState) {
        self.appState = appState
        hkLog("[HotkeyManager] start() trusted=\(AXIsProcessTrusted())")

        if AXIsProcessTrustedWithOptions(nil) {
            tryInstallEventTap()
            appState.hotkeyStatus = eventTap != nil
                ? .installed
                : .tapFailed(reason: "CGEvent.tapCreate returned nil despite Accessibility being trusted")
        } else {
            appState.hotkeyStatus = .accessibilityMissing
            startAccessibilityPolling()
        }

        // macOS disables event taps during sleep — reinstall on wake.
        wakeObserver = NSWorkspace.shared.notificationCenter.addObserver(
            forName: NSWorkspace.didWakeNotification,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            Task { @MainActor in
                guard let self else { return }
                hkLog("[HotkeyManager] system woke — refreshing event tap")
                if self.eventTap != nil {
                    CGEvent.tapEnable(tap: self.eventTap!, enable: true)
                } else if AXIsProcessTrustedWithOptions(nil) {
                    self.tryInstallEventTap()
                    if self.eventTap != nil { self.appState?.hotkeyStatus = .installed }
                }
            }
        }

        // Seed the command-combo state machine with the current binding
        // (if any) and refresh whenever the user changes it. The state
        // machine itself drives press/release detection from the same
        // CGEventTap callback that handles dictation — no Carbon hotkey,
        // no polling.
        commandComboState.setCombo(appState.commandHotkey)
        appState.$commandHotkey
            .receive(on: DispatchQueue.main)
            .sink { [weak self] newValue in
                self?.commandComboState.setCombo(newValue)
                hkLog("[CmdHotkey] state machine rebound to \(newValue?.displayString ?? "<nil>")")
            }
            .store(in: &cancellables)

        // Same for the optional meeting start/stop hotkey.
        meetingComboState.setCombo(appState.meetingHotkey)
        appState.$meetingHotkey
            .receive(on: DispatchQueue.main)
            .sink { [weak self] newValue in
                self?.meetingComboState.setCombo(newValue)
                hkLog("[MtgHotkey] state machine rebound to \(newValue?.displayString ?? "<nil>")")
            }
            .store(in: &cancellables)
    }

    private var cancellables = Set<AnyCancellable>()

    func stop() {
        stopAmplitudePolling()
        uninstallEventTap()
        accessibilityPollTimer?.invalidate()
        accessibilityPollTimer = nil
        if let wakeObserver {
            NSWorkspace.shared.notificationCenter.removeObserver(wakeObserver)
        }
        wakeObserver = nil
        commandComboState.reset()
        meetingComboState.reset()
        cancellables.removeAll()
        appState?.hotkeyStatus = .uninstalled
    }

    // MARK: - Dedicated Command-Mode hotkey

    /// Press handler for the dedicated Command-Mode hotkey. Mirrors the
    /// dictation `handlePress` but always sets `oneShotCommandPending`
    /// so the next stop-and-paste cycle goes through
    /// `stopAndCommandTransform`. Honours `appState.preferredMode` —
    /// the command hotkey shares the dictation Toggle vs Push-to-Talk
    /// behaviour, exactly like Win's command hotkey shares Win's
    /// `ShortcutMode`. Suppressed while a meeting recording owns the
    /// cpal buffer (same gate as the dictation hotkey).
    @MainActor
    private func handleCommandPress() {
        guard let appState else { return }
        if DimmyCore.shared.meetingIsActive {
            hkLog("[CmdHotkey] suppressed — meeting active")
            return
        }
        commandLastPressTime = Date()

        // Already-recording press → finish + command-transform regardless
        // of which mode started the recording. The one-shot flag drives
        // stopRecordingIfNeeded down the command branch even if
        // `commandMode` (sticky) is off.
        if case .recording = appState.recordingState {
            appState.oneShotCommandPending = true
            stopRecordingIfNeeded()
            return
        }

        // Idle press → start a fresh recording in one-shot command mode.
        // Mode comes from `preferredMode`: Push-to-Talk records while the
        // chord is held; Toggle keeps it on until the next press.
        appState.oneShotCommandPending = true
        let mode: RecordingMode = appState.preferredMode == .toggle ? .toggle : .pushToTalk
        startRecording(mode: mode)
    }

    /// Release handler for the dedicated Command-Mode hotkey. Only
    /// meaningful in Push-to-Talk: stops the recording when the chord
    /// is released, with a minimum-hold guard so a stray tap doesn't
    /// transcribe accidental noise. Mirror of dictation `handleRelease`.
    @MainActor
    private func handleCommandRelease() {
        guard let appState else { return }
        guard case .recording(.pushToTalk) = appState.recordingState else { return }
        if let pressTime = commandLastPressTime,
           Date().timeIntervalSince(pressTime) < minimumHoldDuration {
            // Too short — cancel, don't transcribe (and don't fire the
            // command-transform path either: oneShotCommandPending was
            // set on press; cancelRecording clears it via stopRecording
            // → idle, but we need to clear it explicitly to keep state
            // tidy when the user releases too fast).
            appState.oneShotCommandPending = false
            cancelRecording()
            return
        }
        stopRecordingIfNeeded()
    }

    private func startAccessibilityPolling() {
        guard accessibilityPollTimer == nil else { return }
        accessibilityPollTimer = Timer.scheduledTimer(withTimeInterval: 2.0, repeats: true) { [weak self] _ in
            Task { @MainActor in
                guard let self, self.eventTap == nil else { return }
                if AXIsProcessTrustedWithOptions(nil) {
                    hkLog("[HotkeyManager] Accessibility now trusted — installing event tap")
                    self.tryInstallEventTap()
                    if self.eventTap != nil {
                        self.appState?.hotkeyStatus = .installed
                        self.accessibilityPollTimer?.invalidate()
                        self.accessibilityPollTimer = nil
                    }
                }
            }
        }
    }

    private func tryInstallEventTap() {
        if eventTap != nil { return }
        if installEventTap() {
            hkLog("[HotkeyManager] CGEventTap installed (.cgSessionEventTap) — shortcut events will be consumed from other apps")
        } else {
            hkLog("[HotkeyManager] CGEventTap install FAILED (Accessibility not granted). Override disabled.")
        }
    }

    // MARK: - CGEventTap (active, consumes events globally)

    private func installEventTap() -> Bool {
        // Mask covers three event types so the SAME tap can drive:
        //   • dictation (modifier-only chord, .flagsChanged)
        //   • command hotkey modifier-only chord (.flagsChanged)
        //   • command hotkey mod+key chord activation (.keyDown)
        //   • command hotkey mod+key chord release (.keyUp)
        // This mirrors the Win-side single-hook design and replaces the
        // Carbon `RegisterEventHotKey` path for the command hotkey.
        let mask: CGEventMask = (1 << CGEventType.flagsChanged.rawValue)
            | (1 << CGEventType.keyDown.rawValue)
            | (1 << CGEventType.keyUp.rawValue)
        let selfPtr = Unmanaged.passUnretained(self).toOpaque()

        let callback: CGEventTapCallBack = { _, type, event, userInfo in
            guard let userInfo = userInfo else { return Unmanaged.passUnretained(event) }
            let manager = Unmanaged<HotkeyManager>.fromOpaque(userInfo).takeUnretainedValue()

            // Re-enable if system suspended the tap (timeout or user input)
            if type == .tapDisabledByTimeout || type == .tapDisabledByUserInput {
                hkLog("[HotkeyManager] tap disabled type=\(type.rawValue) — re-enabling")
                if let tap = MainActor.assumeIsolated({ manager.eventTap }) {
                    CGEvent.tapEnable(tap: tap, enable: true)
                }
                return Unmanaged.passUnretained(event)
            }

            switch type {
            case .flagsChanged:
                hkLog("[HotkeyManager] TAP flagsChanged, rawFlags=0x\(String(event.flags.rawValue, radix: 16))")
                let flags = NSEvent.ModifierFlags(cgFlags: event.flags)
                let shouldConsume = MainActor.assumeIsolated {
                    manager.handleFlagsAll(flags)
                }
                return shouldConsume ? nil : Unmanaged.passUnretained(event)

            case .keyDown:
                let flags = NSEvent.ModifierFlags(cgFlags: event.flags)
                let keyCode = UInt16(event.getIntegerValueField(.keyboardEventKeycode))
                let shouldConsume = MainActor.assumeIsolated {
                    manager.handleCommandKeyDown(keyCode: keyCode, flags: flags)
                }
                return shouldConsume ? nil : Unmanaged.passUnretained(event)

            case .keyUp:
                let keyCode = UInt16(event.getIntegerValueField(.keyboardEventKeycode))
                let shouldConsume = MainActor.assumeIsolated {
                    manager.handleCommandKeyUp(keyCode: keyCode)
                }
                return shouldConsume ? nil : Unmanaged.passUnretained(event)

            default:
                return Unmanaged.passUnretained(event)
            }
        }

        // .cgSessionEventTap runs at login-session level (no root required, only Accessibility).
        // Active taps at this point can modify or suppress events before they reach other apps.
        guard let tap = CGEvent.tapCreate(
            tap: .cgSessionEventTap,
            place: .headInsertEventTap,
            options: .defaultTap,
            eventsOfInterest: mask,
            callback: callback,
            userInfo: selfPtr
        ) else {
            return false
        }

        let source = CFMachPortCreateRunLoopSource(kCFAllocatorDefault, tap, 0)
        CFRunLoopAddSource(CFRunLoopGetMain(), source, .commonModes)
        CGEvent.tapEnable(tap: tap, enable: true)

        self.eventTap = tap
        self.runLoopSource = source
        return true
    }

    private func uninstallEventTap() {
        if let tap = eventTap {
            CGEvent.tapEnable(tap: tap, enable: false)
        }
        if let source = runLoopSource {
            CFRunLoopRemoveSource(CFRunLoopGetMain(), source, .commonModes)
        }
        eventTap = nil
        runLoopSource = nil
    }

    /// Top-level flagsChanged dispatcher. Forwards the event to BOTH the
    /// dictation handler (which owns the modifier-only dictation chord)
    /// AND the command-combo state machine (which needs to see modifier
    /// transitions for modifier-only command chords AND to release a
    /// mod+key chord when its modifiers drop). Consumes the event when
    /// EITHER consumer wants to (a fired command chord should never
    /// leak its modifier flag flip to the focused app, same as
    /// dictation).
    @discardableResult
    private func handleFlagsAll(_ rawFlags: NSEvent.ModifierFlags) -> Bool {
        let dictConsume = handleFlags(rawFlags)
        let cmdEvent = commandComboState.processFlags(rawFlags)
        let cmdConsume = dispatchCommandEvent(cmdEvent)
        let mtgConsume = dispatchMeetingEvent(meetingComboState.processFlags(rawFlags))
        return dictConsume || cmdConsume || mtgConsume
    }

    /// keyDown handler — feeds the command-combo state machine. Only the
    /// command hotkey listens on keyDown; the dictation chord is
    /// modifier-only and never fires here. Consumed when the chord
    /// activates so the focused app never sees the letter (e.g. an X in
    /// a text field while the user holds ⌃⇧X).
    @discardableResult
    private func handleCommandKeyDown(keyCode: UInt16, flags: NSEvent.ModifierFlags) -> Bool {
        let event = commandComboState.processKeyDown(keyCode: keyCode, flags: flags)
        let cmdConsume = dispatchCommandEvent(event)
        let mtgConsume = dispatchMeetingEvent(meetingComboState.processKeyDown(keyCode: keyCode, flags: flags))
        return cmdConsume || mtgConsume
    }

    /// keyUp handler — also for the command-combo state machine. Fires
    /// the release branch for Push-to-Talk command hotkeys whose
    /// non-modifier key is being lifted while the modifiers are still
    /// held. Consumes the event on release for symmetry with keyDown
    /// consumption so a downstream app can't see half of the chord.
    @discardableResult
    private func handleCommandKeyUp(keyCode: UInt16) -> Bool {
        let event = commandComboState.processKeyUp(keyCode: keyCode)
        let cmdConsume = dispatchCommandEvent(event)
        let mtgConsume = dispatchMeetingEvent(meetingComboState.processKeyUp(keyCode: keyCode))
        return cmdConsume || mtgConsume
    }

    /// Map a `CommandComboState` event onto the press/release handlers
    /// and return whether the OS event should be consumed.
    @discardableResult
    private func dispatchCommandEvent(_ event: CommandComboState.Event) -> Bool {
        switch event {
        case .none:
            return false
        case .pressed:
            handleCommandPress()
            return true
        case .released:
            handleCommandRelease()
            return true
        }
    }

    /// Map a meeting-combo event onto the toggle. TOGGLE-only: the pressed
    /// edge fires the consent-gated start/stop; the release is consumed (for
    /// chord symmetry so a downstream app never sees half a chord) but takes
    /// no action — a meeting can't be push-to-talk.
    @discardableResult
    private func dispatchMeetingEvent(_ event: CommandComboState.Event) -> Bool {
        switch event {
        case .none:
            return false
        case .pressed:
            handleMeetingToggle()
            return true
        case .released:
            return true
        }
    }

    /// Meeting hotkey press → the single consent-gated toggle shared with the
    /// status-bar / Dock / pill menu actions.
    @MainActor
    private func handleMeetingToggle() {
        guard let appState else { return }
        MeetingShortcut.toggle(appState: appState)
    }

    /// Returns true if the event should be consumed (i.e. not forwarded to other apps).
    @discardableResult
    private func handleFlags(_ rawFlags: NSEvent.ModifierFlags) -> Bool {
        let flags = rawFlags.intersection(.deviceIndependentFlagsMask)
        guard let appState else { return false }
        let onlyControlOption = appState.shortcut.matches(flags: flags)
        hkLog("[HotkeyManager] flagsChanged raw=0x\(String(flags.rawValue, radix: 16)) fn=\(flags.contains(.function)) ctrl=\(flags.contains(.control)) opt=\(flags.contains(.option)) cmd=\(flags.contains(.command)) shift=\(flags.contains(.shift)) matchesShortcut=\(onlyControlOption) storedShortcut=\(appState.shortcut.displayString) controlOptionDown=\(controlOptionDown)")

        // Consume if shortcut is either pressed-and-matching or being released from a pressed state
        let consume = onlyControlOption || controlOptionDown

        if onlyControlOption && !controlOptionDown {
            // Shortcut just pressed
            controlOptionDown = true
            lastPressTime = Date()
            handlePress()
        } else if !onlyControlOption && controlOptionDown {
            // Shortcut just released
            controlOptionDown = false
            handleRelease()
        }

        return consume
    }

    private func handlePress() {
        guard let appState else { return }

        // If already recording in toggle mode, stop it
        if case .recording(.toggle) = appState.recordingState {
            stopRecordingIfNeeded()
            lastReleaseTime = nil
            return
        }

        // Use the user's preferred mode
        if appState.preferredMode == .toggle {
            // Toggle mode: press starts, press again stops
            startRecording(mode: .toggle)
        } else {
            // Push-to-talk: press starts, release stops
            startRecording(mode: .pushToTalk)
        }
    }

    private func handleRelease() {
        guard let appState else { return }

        // Only stop push-to-talk on release (toggle stays active until next press)
        if case .recording(.pushToTalk) = appState.recordingState {
            // Check minimum hold duration to avoid accidental triggers
            if let pressTime = lastPressTime, Date().timeIntervalSince(pressTime) < minimumHoldDuration {
                // Too short — cancel, don't transcribe
                cancelRecording()
                return
            }
            stopRecordingIfNeeded()
        }
    }

    // MARK: - Recording via Rust FFI

    private func startRecording(mode: RecordingMode) {
        guard let appState else { return }
        guard case .idle = appState.recordingState else { return }

        // Phase 4 hotkey gate: a meeting recording owns the cpal buffer.
        // Starting a parallel dictation here would corrupt both sessions
        // (verified on Win shipping). Same protection mirrors
        // MeetingWindow.OnHotkey on the Win side.
        if DimmyCore.shared.meetingIsActive {
            hkLog("[HotkeyManager] dictation hotkey ignored — meeting active")
            return
        }

        // Capture the foreground app BEFORE start_recording so the
        // current_app_context slot in Rust state holds the right
        // bundle id by the time the LLM-enhance step reads it.
        let bundleId = AppContextCapture.foregroundBundleId()
        DimmyCore.shared.setAppContext(bundleId: bundleId)
        currentRecordingBundleId = bundleId
        recordingStartedAt = Date()
        // Snapshot the running app reference too: Command Mode's
        // LLM call can take several seconds, during which the user
        // may Cmd+Tab. We restore foreground twice (before Cmd+C and
        // before paste) so the synthetic copy + paste always land on
        // the originally-targeted app. Mirrors Win
        // RestoreTargetForegroundAsync in App.xaml.cs.
        recordingTargetApp = NSWorkspace.shared.frontmostApplication

        let result = DimmyCore.shared.startRecording()
        if result == 0 {
            appState.recordingState = .recording(mode)
            startAmplitudePolling()
        } else if result == -1 {
            appState.lastError = "No API key configured"
            print("[HotkeyManager] startRecording failed: no API key")
            // Failed start → clear so we don't carry stale context into
            // the next attempt.
            DimmyCore.shared.clearAppContext()
            currentRecordingBundleId = ""
            recordingStartedAt = nil
            recordingTargetApp = nil
            appState.oneShotCommandPending = false
        } else if result == -2 {
            print("[HotkeyManager] startRecording failed: already recording")
            DimmyCore.shared.clearAppContext()
            currentRecordingBundleId = ""
            recordingStartedAt = nil
            recordingTargetApp = nil
            appState.oneShotCommandPending = false
        } else if result == -7 {
            // Meeting active — race with the pre-flight meetingIsActive
            // gate above. Silent no-op: don't pull the user out of their
            // meeting context with an error toast, just log.
            hkLog("[HotkeyManager] dictation hotkey suppressed: meeting active (rc=-7)")
            DimmyCore.shared.clearAppContext()
            currentRecordingBundleId = ""
            recordingStartedAt = nil
            recordingTargetApp = nil
            appState.oneShotCommandPending = false
        }
    }

    /// Bundle ID captured at hotkey-down. Held until the post-LLM update
    /// step so we know what to write into history.app_bundle_id.
    private var currentRecordingBundleId: String = ""
    /// Wall-clock start of the active recording. Used by the auto-recap
    /// gate (Phase 6.4) — only fire when the dictation ran long enough
    /// to be worth summarising.
    private var recordingStartedAt: Date?
    /// Frontmost running-application reference captured at hotkey-down.
    /// Used by Command Mode to restore foreground twice (before the
    /// synthetic Cmd+C and before the paste) so a focus drift during
    /// the LLM call doesn't send the result to the wrong app.
    private var recordingTargetApp: NSRunningApplication?

    private func stopRecordingIfNeeded() {
        guard let appState else { return }
        guard appState.isRecording else { return }

        stopAmplitudePolling()
        appState.recordingState = .transcribing

        let startedAt = recordingStartedAt
        recordingStartedAt = nil

        // Captured at hotkey-down (or pill-record). Restored before the paste
        // so clicking the pill Stop — which makes the pill the key window —
        // doesn't make the synthetic Cmd+V land on the pill instead of the
        // app the user was dictating into. Mirror of Win's _targetContext
        // restore, shared with Command Mode (see stopAndCommandTransform).
        let target = recordingTargetApp

        // Command Mode branch: transform the user's selected text with the
        // spoken instruction instead of dictating. Triggered by EITHER
        // the sticky menu toggle (`commandMode`) OR the one-shot
        // dedicated command hotkey (`oneShotCommandPending`).
        // stopAndCommandTransform clears `oneShotCommandPending` itself
        // at the end of the cycle.
        if appState.commandMode || appState.oneShotCommandPending {
            Task { @MainActor in await self.stopAndCommandTransform() }
            return
        }

        // Stop recording + transcribe on background thread (blocking call)
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            let transcript: String
            switch DimmyCore.shared.stopRecording() {
            case .skipped:
                // A concurrent stop (toggle hotkey + pill Stop, or a double
                // trigger) won the core guard and owns the transcript + its
                // delivery. Do NOTHING — no state reset, no paste — or we
                // tear down the UI the winning call is driving. Mirror of
                // Win's `if (result.IsSkipped) return;`.
                print("[HotkeyManager] stop skipped (rc -9) — concurrent stop owns it")
                return
            case .failed(let code):
                print("[HotkeyManager] stop failed rc=\(code)")
                DispatchQueue.main.async { [weak self] in
                    self?.appState?.recordingState = .idle
                }
                return
            case .transcript(let t):
                transcript = t
            }

            // LLM enhancement (if enabled, also blocking)
            var finalText = transcript
            var didEnhance = false
            if !transcript.isEmpty {
                let enhanced = DimmyCore.shared.processWithLLM(text: transcript)
                if !enhanced.isEmpty {
                    finalText = enhanced
                    didEnhance = enhanced != transcript
                }
            }

            // Post-LLM history hook: backfill enhanced_text on the row
            // dimmy_stop_recording just inserted (mirror of Win
            // TranscriptionService.UpdateEnhancedAsync). Runs only when
            // an actual rewrite happened — no point storing duplicate
            // text otherwise. dimmy_history_recent(1) returns the row
            // we just inserted.
            var historyId: Int32 = -1
            if didEnhance, let recent = DimmyCore.shared.historyRecent(limit: 1)?.first,
               let id = recent["id"] as? Int32 ?? (recent["id"] as? Int).map(Int32.init) {
                historyId = id
                DimmyCore.shared.historyUpdateEnhanced(id: id, text: finalText)
            }

            // Capture state we need for the post-paste auto-recap gate
            // before we hand back to the main thread.
            let didEnhanceFinal = didEnhance
            let historyIdFinal = historyId

            // Clear the app-context snapshot so it can't bleed into the
            // next recording. Done on the background thread to avoid
            // racing with another hotkey-down on the main thread.
            DimmyCore.shared.clearAppContext()

            DispatchQueue.main.async { [weak self] in
                guard let self, let appState = self.appState else { return }
                self.currentRecordingBundleId = ""

                appState.lastTranscript = finalText

                if finalText.isEmpty {
                    appState.recordingState = .idle
                    return
                }

                appState.recordingState = .completing

                // Inject text (paste into active app)
                if !appState.keepInClipboard {
                    // Restore the recorded target to the foreground BEFORE the
                    // synthetic Cmd+V. If the user stopped via the pill Stop
                    // button, the pill is now the key window and a naive paste
                    // would land on it. restoreForeground is a no-op when the
                    // target is already frontmost (hotkey-stop case).
                    Task { @MainActor in
                        await self.restoreForeground(target)
                        TextInjector.shared.injectText(finalText)
                    }
                } else {
                    // Just copy to clipboard without pasting
                    NSPasteboard.general.clearContents()
                    NSPasteboard.general.setString(finalText, forType: .string)
                }

                // Phase 6.4 auto-recap: if this dictation ran longer
                // than the user's threshold, fire-and-forget a recap
                // call in the background and append it to enhanced_text.
                // Stays out of the paste critical path — the user gets
                // their transcript first, and the recap quietly lands
                // in the history detail when the LLM returns.
                if didEnhanceFinal, historyIdFinal > 0,
                   let started = startedAt {
                    self.maybeAutoRecap(
                        appState: appState,
                        historyId: historyIdFinal,
                        baseText: finalText,
                        startedAt: started
                    )
                }

                // Return to idle after brief completion animation
                DispatchQueue.main.asyncAfter(deadline: .now() + 1.0) {
                    if case .completing = appState.recordingState {
                        appState.recordingState = .idle
                    }
                }
            }
        }
    }

    // MARK: - Command Mode

    /// Command Mode stop path: grab the user's selected text, transform it
    /// with the spoken instruction, paste the result over the selection.
    /// Falls back to plain dictation when nothing was selected. Mirror of
    /// Win's `App.StopAndCommandTransform`.
    @MainActor
    private func stopAndCommandTransform() async {
        guard let appState else { return }

        let target = recordingTargetApp

        // Restore foreground BEFORE the synthetic Cmd+C — by now the
        // pill window may be the key window (waveform animation
        // tap-target) and the keystroke would land on it. Mirror of
        // Win RestoreTargetForegroundAsync before SelectionCaptureService.
        await restoreForeground(target)

        // Grab the selection while the target app still has focus
        // (synthetic Cmd+C round-trip; nil when nothing is selected).
        let selection = await SelectionCaptureFlow.capture()
        NSLog("[CmdMode] captured selection: \(selection != nil ? "\(selection!.count) chars" : "none")")
        if selection == nil {
            // Say it out loud: without this, a selection that silently
            // failed to capture (AX denied, slow app, secure input) is
            // indistinguishable from intentional generate-at-cursor use,
            // and the LLM result reads like a mis-fired dictation
            // (colleague report, 2026-07-03). Mirror of Win
            // ShowCommandNoSelection.
            DictToastWindow.show(
                kind: .workflowHint,
                title: "Command Mode: no text selected",
                body: "Executing your instruction and inserting the result at the cursor."
            )
        }

        DispatchQueue.global(qos: .userInitiated).async {
            // Always stop recording so the session ends even with no
            // selection. The spoken text is taken RAW (it's the
            // instruction, not content to enhance). A concurrent-stop
            // skip (rc -9) or a failure yields no instruction → empty,
            // which the empty-transcript branch below handles as a no-op.
            let spoken: String
            switch DimmyCore.shared.stopRecording() {
            case .transcript(let t):
                spoken = t.trimmingCharacters(in: .whitespacesAndNewlines)
            case .skipped, .failed:
                spoken = ""
            }
            DimmyCore.shared.clearAppContext()

            var resolved: String?
            var resolvedRc: Int32 = 0
            if !spoken.isEmpty {
                // Pass through whatever was captured — empty string is a
                // valid selection now. The Rust core's
                // `dimmy_command_transform` branches at the prompt-build
                // layer: with selection -> transform/replace; without ->
                // generate text to insert at the caret. Mirror of Win
                // App.StopAndCommandTransform after PR #98 (the OPTIONAL
                // selection contract). Removes the prior "paste raw
                // spoken text on no selection" Mac fallback.
                let sel = selection ?? ""
                let t = DimmyCore.shared.commandTransform(selection: sel, spoken: spoken)
                resolved = t.text
                resolvedRc = t.rc
            }
            let wasEmpty = spoken.isEmpty
            let finalText = resolved
            let finalRc = resolvedRc

            DispatchQueue.main.async { [weak self] in
                guard let self, let appState = self.appState else { return }
                // One-shot dedicated-hotkey flag clears here regardless of
                // outcome — the cycle is over. The sticky menu toggle is
                // untouched (it stays as the user set it).
                appState.oneShotCommandPending = false
                if wasEmpty {
                    appState.recordingState = .idle
                    self.recordingTargetApp = nil
                    return
                }
                guard let text = finalText, !text.isEmpty else {
                    // Every failure gets a toast — until 2026-07-03 only
                    // rc -3 did, so a broken subscription CLI (rc -2) or a
                    // missing local model (rc -4) silently pasted NOTHING
                    // and users concluded command mode was broken.
                    NSLog("[CmdMode] transform failed rc=\(finalRc)")
                    switch finalRc {
                    case -3:
                        DictToastWindow.show(
                            kind: .error,
                            title: "Command Mode needs an LLM key",
                            body: "Open Settings → Providers & keys and connect a provider."
                        )
                    case -4:
                        DictToastWindow.show(
                            kind: .error,
                            title: "Command Mode needs the local model",
                            body: "The local AI model is not downloaded. Get it in Settings → AI cleanup, or switch to a cloud provider."
                        )
                    default:
                        DictToastWindow.show(
                            kind: .error,
                            title: "Command Mode failed",
                            body: "The AI request did not complete (code \(finalRc)). Check Settings → Providers & keys, your connection, or your subscription login."
                        )
                    }
                    appState.recordingState = .idle
                    self.recordingTargetApp = nil
                    return
                }
                appState.lastTranscript = text
                appState.recordingState = .completing
                // Second foreground restore — the LLM round-trip may have
                // taken seconds and the user could have switched apps.
                // Mirror of the second Win RestoreTargetForegroundAsync.
                Task { @MainActor in
                    await self.restoreForeground(target)
                    if !appState.keepInClipboard {
                        TextInjector.shared.injectText(text)
                    } else {
                        NSPasteboard.general.clearContents()
                        NSPasteboard.general.setString(text, forType: .string)
                    }
                    self.recordingTargetApp = nil
                    DispatchQueue.main.asyncAfter(deadline: .now() + 1.0) {
                        if case .completing = appState.recordingState {
                            appState.recordingState = .idle
                        }
                    }
                }
            }
        }
    }

    /// Bring the originally-targeted app back to the foreground if it has
    /// drifted. No-op when nil (no recording target captured) or when the
    /// target is already frontmost. Waits ~80 ms so AppKit's main-runloop
    /// foreground switch lands before the next synthetic keystroke. Same
    /// pattern as Win RestoreTargetForegroundAsync.
    @MainActor
    private func restoreForeground(_ target: NSRunningApplication?) async {
        guard let target, !target.isTerminated else { return }
        if NSWorkspace.shared.frontmostApplication?.processIdentifier == target.processIdentifier {
            return
        }
        target.activate(options: [.activateIgnoringOtherApps])
        try? await Task.sleep(nanoseconds: 80_000_000)
    }

    // MARK: - Phase 6.4 — auto-recap

    /// Fire-and-forget recap pass for a long dictation. Mirrors the
    /// Win-side `MaybeAutoRecap`. Skipped when:
    ///   - threshold is 0 (user opted out)
    ///   - elapsed wall time < threshold
    ///   - history row id unknown (no enhance happened)
    /// On success, appends "═════ Recap ═════\n<recap>" to the
    /// enhanced_text column so the History detail panel shows both.
    private func maybeAutoRecap(appState: AppState,
                                 historyId: Int32,
                                 baseText: String,
                                 startedAt: Date) {
        let threshold = TimeInterval(appState.autoRecapThresholdSecs)
        if threshold <= 0 { return }
        let elapsed = Date().timeIntervalSince(startedAt)
        if elapsed < threshold { return }

        let prompt = """
        Summarise the following dictation in 2-4 short bullet points. \
        Focus on decisions, action items, and key facts. Do NOT include \
        meta-commentary like "Here's a summary"; output the bullets only.

        Dictation:
        \(baseText)
        """

        DispatchQueue.global(qos: .utility).async {
            let modelOverride = pickRecapModel()
            let result = DimmyCore.shared.llmCallRaw(
                prompt: prompt,
                modelOverride: modelOverride,
                maxTokens: 1024
            )
            switch result {
            case .success(let recap):
                let trimmed = recap.trimmingCharacters(in: .whitespacesAndNewlines)
                if trimmed.isEmpty { return }
                let combined = "\(baseText)\n\n═════ Recap ═════\n\(trimmed)"
                _ = DimmyCore.shared.historyUpdateEnhanced(id: historyId, text: combined)
                hkLog("[HotkeyManager] auto-recap saved (id=\(historyId), \(trimmed.count) chars, \(Int(elapsed))s elapsed)")
            case .failure(let err):
                hkLog("[HotkeyManager] auto-recap skipped: \(err)")
            }
        }
    }

    private func cancelRecording() {
        guard let appState else { return }
        guard appState.isRecording else { return }

        stopAmplitudePolling()
        DimmyCore.shared.cancelRecording()
        DimmyCore.shared.clearAppContext()
        currentRecordingBundleId = ""
        recordingStartedAt = nil
        appState.recordingState = .idle
    }

    func stopToggleRecording() {
        stopRecordingIfNeeded()
    }

    /// UI-driven recording toggle, independent of the global hotkey.
    /// Lets the menubar / dock / pill click start a recording even when
    /// Accessibility is missing (CGEventTap dead). Recording itself
    /// still needs Mic — that prompt fires once cpal opens the input.
    func toggleRecordingFromUI() {
        guard let appState else { return }
        if appState.isRecording {
            stopRecordingIfNeeded()
        } else if case .idle = appState.recordingState {
            startRecording(mode: .toggle)
        }
    }

    // MARK: - Amplitude Polling (drives waveform animation)

    private func startAmplitudePolling() {
        amplitudeTimer?.invalidate()
        // Poll at ~12fps (matching the mockup's waveform animation rate)
        amplitudeTimer = Timer.scheduledTimer(withTimeInterval: 1.0 / 12.0, repeats: true) { [weak self] _ in
            Task { @MainActor in
                self?.updateWaveformLevels()
            }
        }
    }

    private func stopAmplitudePolling() {
        amplitudeTimer?.invalidate()
        amplitudeTimer = nil
    }

    private func updateWaveformLevels() {
        guard let appState, appState.isRecording else { return }

        let rawAmplitude = DimmyCore.shared.getAmplitude()
        // Boost: raw mic amplitude is typically 0.02-0.3 peak.
        // Scale up so normal speech fills 40-80% of the bars.
        let amplitude = min(CGFloat(rawAmplitude) * 5.0, 1.0)

        // Generate 7 waveform bars from amplitude value
        // Add slight variation per bar for visual interest
        var levels: [CGFloat] = []
        for i in 0..<7 {
            let variation = CGFloat.random(in: 0.7...1.3)
            let centerBias: CGFloat = 1.0 - abs(CGFloat(i) - 3.0) / 5.0 // center bars taller
            let level = amplitude * variation * centerBias
            levels.append(max(0.08, min(1.0, level)))
        }

        appState.waveformLevels = levels
    }
}

// MARK: - CommandComboState

/// Swift port of the Win `Binding` state machine for the dedicated
/// Command-Mode hotkey. Tracks whether the configured `combo` is
/// currently active (all required keys held), driven by the three
/// `CGEventTap` callbacks: `.flagsChanged`, `.keyDown`, `.keyUp`.
///
/// Models both supported combo shapes the Rust hook accepts:
///   - modifier-only (e.g. ⌃⌥, ⌘⌥): activation = all modifiers held;
///     release = any modifier dropped. Driven entirely by flagsChanged.
///   - modifier+key (e.g. ⌃⇧X): activation = modifiers held AND keyDown
///     matches; release = matching keyUp OR any modifier dropped. Driven
///     by keyDown/keyUp + flagsChanged for the modifier-drop branch.
///
/// Mirrors the Rust `Binding::process` semantics: the binding fires on
/// the FIRST transition into "all keys held" and is idempotent during
/// hold (auto-repeats are ignored — the OS sends repeated keyDowns).
///
/// Pure-state — does NOT touch AppState / DimmyCore. The owning
/// `HotkeyManager` consumes the returned `Event` and runs the press /
/// release business logic. Lives at file scope so it's directly
/// unit-testable without spinning up the CGEventTap.
final class CommandComboState {
    enum Event {
        case none
        case pressed
        case released
    }

    private var combo: HotkeyCombo?
    private var active: Bool = false
    /// When activation is mod+key, the matched keyCode. Used to scope
    /// the keyUp release path to the same physical key.
    private var heldKey: UInt16?

    /// Update the bound combo. Resets internal state — a re-bind while
    /// the prior combo was active means the old physical keys can no
    /// longer release the new combo, so we drop the active flag rather
    /// than risk a stuck "pressed" state.
    func setCombo(_ combo: HotkeyCombo?) {
        self.combo = combo
        self.active = false
        self.heldKey = nil
    }

    /// Tear down: drop any in-flight activation. Called from
    /// `HotkeyManager.stop()` so a future `start()` starts clean.
    func reset() {
        active = false
        heldKey = nil
    }

    /// Process a `.flagsChanged` event. Returns:
    ///   - `.pressed`  : a modifier-only combo just became active
    ///   - `.released` : any active combo lost a required modifier (mod-
    ///                   only deactivation OR mod+key bailing because the
    ///                   user lifted Shift mid-chord)
    ///   - `.none`     : no transition
    ///
    /// `MainActor`-isolated by the calling tap callback, so concurrent
    /// updates aren't a concern.
    func processFlags(_ rawFlags: NSEvent.ModifierFlags) -> Event {
        guard let combo else { return .none }
        let flags = rawFlags.intersection(.deviceIndependentFlagsMask)
        let modsHeld = modifiersMatch(combo: combo, flags: flags)

        if combo.isModifierOnly {
            // Modifier-only combo: STRICT equality on the modifier set.
            // We don't fire on "supersets" because the user might be
            // holding ⌃⌥ during a normal ⌃⌥⇧A chord — we should NOT
            // start a recording for "⌃⌥" in that scenario.
            let strictMatch = modsHeld && modifierCount(flags) == combo.modifierCount
            if strictMatch && !active {
                active = true
                return .pressed
            }
            if !strictMatch && active {
                active = false
                return .released
            }
            return .none
        }

        // Mod+key combo: flagsChanged only matters for the bail-out case
        // where the user releases a modifier while the chord is active.
        // Activation itself fires in processKeyDown.
        if active && !modsHeld {
            active = false
            heldKey = nil
            return .released
        }
        return .none
    }

    /// Process a `.keyDown` event. Returns `.pressed` for the first
    /// keyDown that satisfies the configured mod+key chord (mods held +
    /// key matches), `.none` otherwise. Auto-repeats are ignored —
    /// `active` is already true, so the if-guard short-circuits.
    func processKeyDown(keyCode: UInt16, flags: NSEvent.ModifierFlags) -> Event {
        guard let combo else { return .none }
        if combo.isModifierOnly { return .none }
        if active { return .none }
        guard combo.keyCode == keyCode else { return .none }
        let masked = flags.intersection(.deviceIndependentFlagsMask)
        guard modifiersMatch(combo: combo, flags: masked) else { return .none }
        // For mod+key we require strict equality on the modifier mask —
        // a chord like ⌃⇧X should NOT fire while the user holds
        // ⌃⇧⌘X (which is presumably some OS / app shortcut).
        guard modifierCount(masked) == combo.modifierCount else { return .none }
        active = true
        heldKey = keyCode
        return .pressed
    }

    /// Process a `.keyUp` event. Returns `.released` when the lifted
    /// key matches the one that activated the chord; `.none` otherwise.
    func processKeyUp(keyCode: UInt16) -> Event {
        guard let combo, !combo.isModifierOnly else { return .none }
        guard active, heldKey == keyCode else { return .none }
        active = false
        heldKey = nil
        return .released
    }

    // MARK: - Helpers (internal for unit tests)

    func modifiersMatch(combo: HotkeyCombo, flags: NSEvent.ModifierFlags) -> Bool {
        let masked = flags.intersection(.deviceIndependentFlagsMask)
        if combo.control && !masked.contains(.control) { return false }
        if combo.option && !masked.contains(.option) { return false }
        if combo.command && !masked.contains(.command) { return false }
        if combo.shift && !masked.contains(.shift) { return false }
        return true
    }

    func modifierCount(_ flags: NSEvent.ModifierFlags) -> Int {
        var n = 0
        if flags.contains(.control) { n += 1 }
        if flags.contains(.option) { n += 1 }
        if flags.contains(.command) { n += 1 }
        if flags.contains(.shift) { n += 1 }
        return n
    }

    // Test-only inspection — keeps the unit suite oblivious to the
    // `active` private without weakening encapsulation in production
    // code. `nonisolated` so XCTest can call it from outside @MainActor.
    var isActiveForTesting: Bool { active }
}

/// Single consent-gated entry point to TOGGLE a meeting recording, shared by
/// the meeting hotkey (`HotkeyManager`) and the status-bar / Dock / pill menu
/// actions. Mirror of the Windows `App.ToggleMeetingFromShortcutAsync`.
///
/// - Active meeting → stop + recap via the pill's existing pipeline.
/// - A dictation in flight → ignored (the two captures share the cpal buffer
///   and the core only guards the other direction — it blocks dictation while
///   a meeting runs, not the reverse).
/// - Idle → mandatory consent modal, then start in the BACKGROUND (the Meeting
///   window is NOT opened; the pill + menus reflect state via the
///   `meeting_state` event / `appState.meetingActive`).
///
/// Lives here (a file already in the Xcode target) rather than its own file
/// because the project lists sources explicitly — a new .swift wouldn't build
/// without a project.pbxproj edit.
@MainActor
enum MeetingShortcut {
    static func toggle(appState: AppState, source: String = "hotkey") {
        if DimmyCore.shared.meetingIsActive {
            NSLog("[MeetingShortcut] stopping active meeting (+recap)")
            DimmyCore.shared.trackMeetingAction(source: source)
            PillWindowController.stopMeetingFromPill(appState: appState)
            return
        }
        if appState.isRecording {
            NSLog("[MeetingShortcut] ignored — a dictation is in progress")
            appState.lastError = "Finish the dictation first"
            return
        }
        let lang = Locale.current.language.languageCode?.identifier ?? "en"
        guard MeetingConsentFlow.confirmAndAnnounce(lang: lang) else {
            NSLog("[MeetingShortcut] consent declined")
            return
        }
        if DimmyCore.shared.meetingStart() == nil {
            NSLog("[MeetingShortcut] meeting start failed")
            appState.lastError = "Meeting start failed"
        } else {
            // Pin the recap intent for THIS meeting. A background/shortcut
            // meeting has no per-meeting "Generate recap" toggle (that UI
            // only exists in the meeting window's idle view), so every stop
            // path reads AppState.meetingGenerateRecap — if we don't set it
            // here it keeps a STALE value left over from an earlier
            // window-started meeting and the recap gets silently skipped at
            // stop. Default to the app-wide intent (recap on).
            appState.meetingGenerateRecap = true
            DimmyCore.shared.trackMeetingAction(source: source)
        }
    }
}

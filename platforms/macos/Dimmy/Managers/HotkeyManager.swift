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

    // Carbon hotkey registration for the dedicated one-shot Command-Mode
    // shortcut (`AppState.commandHotkey`). Kept separate from the
    // dictation CGEventTap (flagsChanged) because it's a modifier+key
    // chord, not modifier-only. Mirrors the pattern in
    // DictHotkeyManager.installCarbonHotKey. Carbon-registered hotkeys
    // are consumed system-wide by the registering process so the focused
    // app never sees the combo.
    private var commandCarbonHotKeyRef: EventHotKeyRef?
    private var commandCarbonEventHandler: EventHandlerRef?
    private var lastCommandHotkey: HotkeyCombo?

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

        // Register the dedicated Command-Mode hotkey (if the user has
        // set one) and re-register whenever it changes. Event-driven via
        // Combine sink on the @Published commandHotkey, no polling.
        installCommandCarbonHotKey(for: appState.commandHotkey)
        appState.$commandHotkey
            .receive(on: DispatchQueue.main)
            .sink { [weak self] newValue in
                self?.installCommandCarbonHotKey(for: newValue)
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
        uninstallCommandCarbonHotKey()
        cancellables.removeAll()
        appState?.hotkeyStatus = .uninstalled
    }

    // MARK: - Dedicated Command-Mode hotkey

    /// Carbon bitmask for the modifier set of a `HotkeyCombo`. Carbon
    /// uses its own constants (`cmdKey`, `shiftKey`, …) distinct from
    /// NSEvent.ModifierFlags. Matches DictHotkeyManager.carbonModifiers.
    private func carbonModifiers(_ combo: HotkeyCombo) -> UInt32 {
        var m: UInt32 = 0
        if combo.command { m |= UInt32(cmdKey) }
        if combo.shift   { m |= UInt32(shiftKey) }
        if combo.option  { m |= UInt32(optionKey) }
        if combo.control { m |= UInt32(controlKey) }
        return m
    }

    /// Register (or re-register) the global Carbon hotkey for the
    /// dedicated one-shot Command-Mode shortcut. Idempotent: tears
    /// down any previous registration first. Nil combo = uninstall and
    /// leave nothing registered.
    private func installCommandCarbonHotKey(for combo: HotkeyCombo?) {
        if lastCommandHotkey == combo { return }
        uninstallCommandCarbonHotKey()
        lastCommandHotkey = combo

        guard let combo else { return }
        let mods = carbonModifiers(combo)
        guard mods != 0 else {
            hkLog("[CmdHotkey] refusing to register modifier-less combo")
            return
        }

        // 'DIMC' = "Dimmy Command". Distinct from DictHotkeyManager's
        // 'DIMD' so the two handlers never see each other's events.
        let signature: OSType = 0x44494D43
        let hotKeyID = EventHotKeyID(signature: signature, id: 3)

        var ref: EventHotKeyRef?
        let regStatus = RegisterEventHotKey(
            UInt32(combo.keyCode),
            mods,
            hotKeyID,
            GetApplicationEventTarget(),
            0,
            &ref
        )
        guard regStatus == noErr, let ref = ref else {
            hkLog("[CmdHotkey] RegisterEventHotKey failed status=\(regStatus) — combo may collide with system shortcut")
            return
        }
        self.commandCarbonHotKeyRef = ref

        var eventType = EventTypeSpec(
            eventClass: OSType(kEventClassKeyboard),
            eventKind: UInt32(kEventHotKeyPressed)
        )
        let selfPtr = Unmanaged.passUnretained(self).toOpaque()
        let handler: EventHandlerUPP = { (_, eventRef, userData) -> OSStatus in
            guard let userData = userData, let eventRef = eventRef else { return noErr }
            // Inspect the event so we only react to our signature/id
            // pair — a defensive guard in case the application target
            // ever multiplexes hotkeys we didn't register.
            var hkID = EventHotKeyID()
            let getStatus = GetEventParameter(
                eventRef,
                EventParamName(kEventParamDirectObject),
                EventParamName(typeEventHotKeyID),
                nil,
                MemoryLayout<EventHotKeyID>.size,
                nil,
                &hkID
            )
            guard getStatus == noErr, hkID.signature == 0x44494D43, hkID.id == 3 else {
                return noErr
            }
            let manager = Unmanaged<HotkeyManager>.fromOpaque(userData).takeUnretainedValue()
            Task { @MainActor in manager.handleCommandCarbonHotKey() }
            return noErr
        }

        var handlerRef: EventHandlerRef?
        let installStatus = InstallEventHandler(
            GetApplicationEventTarget(),
            handler,
            1,
            &eventType,
            selfPtr,
            &handlerRef
        )
        if installStatus == noErr {
            self.commandCarbonEventHandler = handlerRef
            hkLog("[CmdHotkey] registered \(combo.displayString)")
        } else {
            hkLog("[CmdHotkey] InstallEventHandler failed status=\(installStatus)")
            UnregisterEventHotKey(ref)
            self.commandCarbonHotKeyRef = nil
        }
    }

    private func uninstallCommandCarbonHotKey() {
        if let ref = commandCarbonHotKeyRef {
            UnregisterEventHotKey(ref)
            commandCarbonHotKeyRef = nil
        }
        if let h = commandCarbonEventHandler {
            RemoveEventHandler(h)
            commandCarbonEventHandler = nil
        }
    }

    /// Fires on the dedicated Command-Mode hotkey. Flips
    /// `oneShotCommandPending` so the next stop-and-paste cycle goes
    /// through `stopAndCommandTransform`. Mirrors the dictation hotkey
    /// "press starts or stops" semantics in handlePress, but always
    /// toggle-style (Carbon hotkeys have no release event). Suppressed
    /// while a meeting recording owns the cpal buffer — same gate as
    /// the dictation hotkey.
    @MainActor
    private func handleCommandCarbonHotKey() {
        guard let appState else { return }
        if DimmyCore.shared.meetingIsActive {
            hkLog("[CmdHotkey] suppressed — meeting active")
            return
        }
        if case .recording = appState.recordingState {
            // Mid-recording press → finish + command-transform. The
            // flag drives stopRecordingIfNeeded down the command branch
            // even if `commandMode` (sticky) is off.
            appState.oneShotCommandPending = true
            stopRecordingIfNeeded()
        } else {
            // Idle press → start a fresh recording in one-shot command
            // mode. Always .toggle: Carbon press has no release event
            // so PTT semantics can't apply.
            appState.oneShotCommandPending = true
            startRecording(mode: .toggle)
        }
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
        let mask: CGEventMask = (1 << CGEventType.flagsChanged.rawValue)
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

            guard type == .flagsChanged else { return Unmanaged.passUnretained(event) }
            hkLog("[HotkeyManager] TAP callback fired, rawFlags=0x\(String(event.flags.rawValue, radix: 16))")

            let flags = NSEvent.ModifierFlags(cgFlags: event.flags)
            // CGEventTap callback runs on the main runloop thread (tap attached to main runloop).
            let shouldConsume = MainActor.assumeIsolated {
                manager.handleFlags(flags)
            }
            return shouldConsume ? nil : Unmanaged.passUnretained(event)
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
        DispatchQueue.global(qos: .userInitiated).async {
            let transcript = DimmyCore.shared.stopRecording() ?? ""

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
                    TextInjector.shared.injectText(finalText)
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

        DispatchQueue.global(qos: .userInitiated).async {
            // Always stop recording so the session ends even with no
            // selection. The spoken text is taken RAW (it's the
            // instruction, not content to enhance).
            let spoken = (DimmyCore.shared.stopRecording() ?? "")
                .trimmingCharacters(in: .whitespacesAndNewlines)
            DimmyCore.shared.clearAppContext()

            var resolved: String?
            var resolvedRc: Int32 = 0
            if !spoken.isEmpty {
                if let sel = selection, !sel.isEmpty {
                    let t = DimmyCore.shared.commandTransform(selection: sel, spoken: spoken)
                    resolved = t.text
                    resolvedRc = t.rc
                } else {
                    // No selection → behave like plain dictation and paste
                    // what the user said (least-surprising fallback).
                    resolved = spoken
                    resolvedRc = 1
                }
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
                    if finalRc == -3 {
                        DictToastWindow.show(
                            kind: .error,
                            title: "Command Mode needs an LLM key",
                            body: "Open Settings → Providers & keys and connect a provider."
                        )
                    } else {
                        NSLog("[CmdMode] transform failed rc=\(finalRc)")
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

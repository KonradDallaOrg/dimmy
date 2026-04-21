import AppKit
import ApplicationServices
import Carbon
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
    }

    func stop() {
        stopAmplitudePolling()
        uninstallEventTap()
        accessibilityPollTimer?.invalidate()
        accessibilityPollTimer = nil
        if let wakeObserver {
            NSWorkspace.shared.notificationCenter.removeObserver(wakeObserver)
        }
        wakeObserver = nil
        appState?.hotkeyStatus = .uninstalled
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

        let result = DimmyCore.shared.startRecording()
        if result == 0 {
            appState.recordingState = .recording(mode)
            startAmplitudePolling()
        } else if result == -1 {
            appState.lastError = "No API key configured"
            print("[HotkeyManager] startRecording failed: no API key")
        } else if result == -2 {
            print("[HotkeyManager] startRecording failed: already recording")
        }
    }

    private func stopRecordingIfNeeded() {
        guard let appState else { return }
        guard appState.isRecording else { return }

        stopAmplitudePolling()
        appState.recordingState = .transcribing

        // Stop recording + transcribe on background thread (blocking call)
        DispatchQueue.global(qos: .userInitiated).async {
            let transcript = DimmyCore.shared.stopRecording() ?? ""

            // LLM enhancement (if enabled, also blocking)
            var finalText = transcript
            if !transcript.isEmpty {
                let enhanced = DimmyCore.shared.processWithLLM(text: transcript)
                if !enhanced.isEmpty {
                    finalText = enhanced
                }
            }

            DispatchQueue.main.async { [weak self] in
                guard let appState = self?.appState else { return }

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

                // Return to idle after brief completion animation
                DispatchQueue.main.asyncAfter(deadline: .now() + 1.0) {
                    if case .completing = appState.recordingState {
                        appState.recordingState = .idle
                    }
                }
            }
        }
    }

    private func cancelRecording() {
        guard let appState else { return }
        guard appState.isRecording else { return }

        stopAmplitudePolling()
        DimmyCore.shared.cancelRecording()
        appState.recordingState = .idle
    }

    func stopToggleRecording() {
        stopRecordingIfNeeded()
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

import AppKit
import Carbon

@MainActor
final class HotkeyManager {
    static let shared = HotkeyManager()

    private var globalFlagsMonitor: Any?
    private var localFlagsMonitor: Any?

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

    private init() {}

    func start(appState: AppState) {
        self.appState = appState

        globalFlagsMonitor = NSEvent.addGlobalMonitorForEvents(matching: .flagsChanged) { [weak self] event in
            Task { @MainActor in
                self?.handleFlagsChanged(event)
            }
        }

        localFlagsMonitor = NSEvent.addLocalMonitorForEvents(matching: .flagsChanged) { [weak self] event in
            Task { @MainActor in
                self?.handleFlagsChanged(event)
            }
            return event
        }
    }

    func stop() {
        stopAmplitudePolling()
        if let m = globalFlagsMonitor { NSEvent.removeMonitor(m) }
        if let m = localFlagsMonitor { NSEvent.removeMonitor(m) }
        globalFlagsMonitor = nil
        localFlagsMonitor = nil
    }

    private func handleFlagsChanged(_ event: NSEvent) {
        let flags = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
        guard let appState else { return }
        let onlyControlOption = appState.shortcut.matches(flags: flags)

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

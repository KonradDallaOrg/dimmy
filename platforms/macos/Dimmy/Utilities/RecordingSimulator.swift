import Foundation

/// Simulates a full recording cycle for testing UI without a microphone.
/// Cycles: idle → recording (3s, animated waveform) → transcribing (1.5s) →
/// processing (1.5s) → completing (1.5s) → idle
@MainActor
enum RecordingSimulator {
    private static var isRunning = false

    static func simulate(appState: AppState) {
        guard !isRunning else { return }
        isRunning = true

        var waveformTimer: Timer?

        // Phase 1: Recording with animated waveform (3 seconds)
        appState.recordingState = .recording(.pushToTalk)
        waveformTimer = Timer.scheduledTimer(withTimeInterval: 1.0 / 12.0, repeats: true) { _ in
            Task { @MainActor in
                var levels: [CGFloat] = []
                for i in 0..<7 {
                    let variation = CGFloat.random(in: 0.7...1.3)
                    let centerBias: CGFloat = 1.0 - abs(CGFloat(i) - 3.0) / 5.0
                    let amplitude = CGFloat.random(in: 0.3...0.9)
                    let level = amplitude * variation * centerBias
                    levels.append(max(0.15, min(1.0, level)))
                }
                appState.waveformLevels = levels
            }
        }

        // Phase 2: Transcribing
        DispatchQueue.main.asyncAfter(deadline: .now() + 3.0) {
            waveformTimer?.invalidate()
            appState.recordingState = .transcribing
        }

        // Phase 3: Processing
        DispatchQueue.main.asyncAfter(deadline: .now() + 4.5) {
            appState.recordingState = .processing
        }

        // Phase 4: Completing
        DispatchQueue.main.asyncAfter(deadline: .now() + 6.0) {
            appState.lastTranscript = "This is a simulated transcription to test the UI flow."
            appState.recordingState = .completing
        }

        // Phase 5: Back to idle
        DispatchQueue.main.asyncAfter(deadline: .now() + 7.5) {
            appState.recordingState = .idle
            isRunning = false
        }
    }
}

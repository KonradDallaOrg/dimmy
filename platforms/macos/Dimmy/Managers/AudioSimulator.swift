import Foundation
import Combine

@MainActor
final class AudioSimulator: ObservableObject {
    static let shared = AudioSimulator()

    private var waveformTimer: Timer?
    private var targetLevels: [CGFloat] = Array(repeating: 0.2, count: 7)
    private var currentLevels: [CGFloat] = Array(repeating: 0.2, count: 7)
    private var targetChangeTimer: Timer?

    private init() {}

    func startRecording(appState: AppState) {
        // Generate new target levels every ~0.4s (organic feel)
        targetChangeTimer = Timer.scheduledTimer(withTimeInterval: 0.4, repeats: true) { [weak self] _ in
            Task { @MainActor in
                self?.targetLevels = (0..<7).map { _ in CGFloat.random(in: 0.15...1.0) }
            }
        }
        targetLevels = (0..<7).map { _ in CGFloat.random(in: 0.15...1.0) }

        // Interpolate toward targets at ~12fps for smooth motion
        waveformTimer = Timer.scheduledTimer(withTimeInterval: 1.0 / 12.0, repeats: true) { [weak self] _ in
            Task { @MainActor in
                guard let self, let appState = self.appStateRef else { return }
                let smoothing: CGFloat = 0.3
                self.currentLevels = zip(self.currentLevels, self.targetLevels).map { current, target in
                    current + (target - current) * smoothing
                }
                appState.waveformLevels = self.currentLevels
            }
        }
        appStateRef = appState
    }

    private weak var appStateRef: AppState?

    func cancelRecording(appState: AppState) {
        stopTimers()
        appState.waveformLevels = Array(repeating: 0.2, count: 7)
        appState.recordingState = .idle
    }

    func stopRecording(appState: AppState) {
        stopTimers()
        appState.waveformLevels = Array(repeating: 0.2, count: 7)

        appState.recordingState = .completing

        DispatchQueue.main.asyncAfter(deadline: .now() + 0.6) {
            TextInjector.shared.injectText(FunnyTexts.random)

            DispatchQueue.main.asyncAfter(deadline: .now() + 0.4) {
                appState.recordingState = .idle
            }
        }
    }

    private func stopTimers() {
        waveformTimer?.invalidate()
        waveformTimer = nil
        targetChangeTimer?.invalidate()
        targetChangeTimer = nil
        currentLevels = Array(repeating: 0.2, count: 7)
        targetLevels = Array(repeating: 0.2, count: 7)
    }
}

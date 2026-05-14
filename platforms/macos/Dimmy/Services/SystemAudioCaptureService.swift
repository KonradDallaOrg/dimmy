import Foundation
import ScreenCaptureKit
import AVFoundation

/// Captures system audio on macOS 14+ via ScreenCaptureKit and forwards
/// f32 PCM samples to Rust via dimmy_push_loopback_audio.
///
/// Lifecycle: call start() after dimmy_meeting_start; stop() before/after
/// dimmy_meeting_stop. If Screen Recording permission is denied, start()
/// returns false and the meeting continues mic-only.
@MainActor
final class SystemAudioCaptureService: NSObject {
    static let shared = SystemAudioCaptureService()
    private var stream: SCStream?
    private var isRunning = false
    private override init() {}

    func start() async -> Bool {
        guard !isRunning else { return true }

        do {
            let content = try await SCShareableContent.excludingDesktopWindows(
                false, onScreenWindowsOnly: false
            )
            guard let display = content.displays.first else { return false }

            let config = SCStreamConfiguration()
            config.capturesAudio = true
            config.excludesCurrentProcessAudioFromMixerService = true
            config.sampleRate = 48_000
            config.channelCount = 1
            // Minimal 2×2 display capture required by SCStream API even for
            // audio-only; GPU cost is negligible at this resolution.
            config.width = 2
            config.height = 2

            let filter = SCContentFilter(display: display, excludingWindows: [])
            let s = SCStream(filter: filter, configuration: config, delegate: nil)
            try s.addStreamOutput(
                self, type: .audio,
                sampleHandlerQueue: .global(qos: .userInteractive)
            )
            try await s.startCapture()
            stream = s
            isRunning = true
            return true
        } catch {
            print("[SystemAudio] start failed: \(error)")
            return false
        }
    }

    func stop() {
        guard isRunning, let s = stream else { return }
        s.stopCapture { _ in }
        stream = nil
        isRunning = false
    }
}

extension SystemAudioCaptureService: SCStreamOutput {
    nonisolated func stream(
        _ stream: SCStream,
        didOutputSampleBuffer sampleBuffer: CMSampleBuffer,
        of type: SCStreamOutputType
    ) {
        guard type == .audio,
              let blockBuffer = sampleBuffer.dataBuffer else { return }

        // Guard: only handle float32 linear PCM (ScreenCaptureKit default).
        if let fmt = sampleBuffer.formatDescription,
           let asbd = fmt.audioStreamBasicDescription,
           (asbd.mFormatFlags & kAudioFormatFlagIsFloat) == 0 {
            return
        }

        var length = 0
        var dataPointer: UnsafeMutablePointer<CChar>?
        guard CMBlockBufferGetDataPointer(
            blockBuffer, atOffset: 0,
            lengthAtOffsetOut: nil, totalLengthOut: &length,
            dataPointerOut: &dataPointer
        ) == noErr, let ptr = dataPointer else { return }

        let sampleCount = length / MemoryLayout<Float>.size
        guard sampleCount > 0 else { return }

        ptr.withMemoryRebound(to: Float.self, capacity: sampleCount) { floatPtr in
            _ = dimmy_push_loopback_audio(floatPtr, Int32(sampleCount), 48_000)
        }
    }
}

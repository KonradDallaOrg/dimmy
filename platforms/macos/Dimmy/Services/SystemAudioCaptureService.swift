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

    /// Sample rate SCStream will be (or is currently) configured at.
    /// Mirrors the cpal mic rate when one is live; falls back through
    /// a cpal probe (no stream opened) and finally to 48 kHz so we
    /// always pick a rate before SCStream is created.
    ///
    /// Resolution order:
    ///   1. `dimmy_get_active_mic_sample_rate()` — the rate the live
    ///      cpal stream is actually running at.
    ///   2. `dimmy_probe_primary_sample_rate()` — what cpal would open
    ///      the selected mic at; closes the race where SCStream starts
    ///      before the cpal Start command has been serviced.
    ///   3. Hardcoded 48 000 Hz when no input device is available
    ///      (SCStream still needs some rate).
    static func plannedSampleRate() -> Int {
        let active = Int(dimmy_get_active_mic_sample_rate())
        if active > 0 { return active }
        let probed = Int(dimmy_probe_primary_sample_rate())
        if probed > 0 { return probed }
        return 48_000
    }

    func start() async -> Bool {
        guard !isRunning else { return true }

        do {
            let content = try await SCShareableContent.excludingDesktopWindows(
                false, onScreenWindowsOnly: false
            )
            guard let display = content.displays.first else { return false }

            let config = SCStreamConfiguration()
            config.capturesAudio = true
            config.excludesCurrentProcessAudio = true
            // Match SCStream's rate to whatever cpal has the mic
            // running at. Hardcoding 48 kHz when the mic is at 16
            // kHz (BT A2DP, Jabra Evolve 3 / AirPods family) forces
            // the macOS audio HAL to renegotiate the global mixer
            // every time recording starts — the user perceives this
            // as a dirty / quieter audio output in their headphones
            // for the duration of the meeting. `plannedSampleRate()`
            // falls back to a non-stream cpal probe when the active
            // stream hasn't published a rate yet (race between
            // `dimmy_meeting_start` and SCStream config), then to
            // 48 kHz only if no input device is available at all.
            let chosenRate = Self.plannedSampleRate()
            config.sampleRate = chosenRate
            config.channelCount = 1
            // Pre-publish the chosen rate so the meeting worker's
            // STT downsample path uses the right source rate even
            // before the first sample buffer's ASBD has been read.
            // The actual rate published on each push (from ASBD)
            // wins if it differs.
            _ = dimmy_set_loopback_sample_rate(Int32(chosenRate))
            NSLog("[SystemAudio] SCStream sampleRate=%d", chosenRate)
            // Minimal 2×2 display capture required by SCStream API even for
            // audio-only; GPU cost is negligible at this resolution.
            config.width = 2
            config.height = 2

            // Exclude Apple's media-content apps from capture so
            // ScreenCaptureKit doesn't trigger the kTCCServiceMediaLibrary
            // / kTCCServicePhotos TCC prompts on macOS Sequoia 15.x.
            // SCKit asks for those grants when `capturesAudio=true` AND
            // one of these apps is currently producing audio — even
            // though our actual goal is to capture meeting / browser /
            // Zoom output, not the user's music library. Dropping the
            // media apps from the capture filter keeps audio_system.wav
            // focused on what users actually want transcribed AND
            // sidesteps the prompts. Bundle ids align with
            // AppContextCapture.mediaAppBundleIds so the same allowlist
            // governs both icon-resolution and audio capture.
            let mediaBundleIds: Set<String> = [
                "com.apple.Music",
                "com.apple.Photos",
                "com.apple.TV",
                "com.apple.Podcasts",
                "com.apple.iTunes",
            ]
            let mediaApps = content.applications.filter {
                mediaBundleIds.contains($0.bundleIdentifier)
            }
            let filter: SCContentFilter
            if mediaApps.isEmpty {
                filter = SCContentFilter(display: display, excludingWindows: [])
            } else {
                filter = SCContentFilter(
                    display: display,
                    excludingApplications: mediaApps,
                    exceptingWindows: []
                )
                NSLog(
                    "[SystemAudio] excluding %d media app(s) from capture filter",
                    mediaApps.count
                )
            }
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

        // Read the actual rate ScreenCaptureKit ships samples at from
        // the sample buffer's ASBD. `SCStreamConfiguration.sampleRate`
        // is a hint that the OS may honour or quietly override (e.g.
        // device renegotiation, route changes). Trusting the hint and
        // hardcoding 48 000 here is exactly what made `audio_system.wav`
        // ship a 48 kHz header while samples were 16 kHz on BT-HFP
        // mics — playback ran at 3× speed and the loopback STT call
        // downsampled against the wrong rate and returned empty.
        var bufferRate: Int32 = 0
        if let fmt = sampleBuffer.formatDescription,
           let asbd = fmt.audioStreamBasicDescription {
            if (asbd.mFormatFlags & kAudioFormatFlagIsFloat) == 0 {
                return
            }
            bufferRate = Int32(asbd.mSampleRate)
        } else {
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
            _ = dimmy_push_loopback_audio(floatPtr, Int32(sampleCount), bufferRate)
        }
    }
}

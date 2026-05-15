import Foundation
import AVFoundation

// MARK: - SampleAudioSynth
//
// Synthesises a 4-second 16 kHz mono WAV with 4 short sine "syllables"
// under a Hann envelope. Sounds like a series of beeps but visually
// mimics real-speech rhythm — amplitude rises and falls across the
// waveform — so the History detail's seek + play UI has something
// concrete to demo without needing a microphone.
//
// Used by the "Seed sample history" debug button in Advanced. Files
// land in `<config>/history_audio/<historyRowId>.wav` so the regular
// retention prune sweeps them like any real recording.

enum SampleAudioSynth {
    /// Where the v2 history audio retention layer expects WAV files.
    /// Mirrors `core/src/lib.rs::history_audio_dir()` — kept as a
    /// freestanding helper so we don't need an FFI getter for a path
    /// that's stable by Application Support convention.
    static func historyAudioDir() -> URL? {
        // Honour the build flavor — staging writes under
        // `dimmy-staging/history_audio/`, prod under `dimmy/`.
        // Hardcoding "dimmy" made the staging "Seed sample history"
        // button write into the prod dir while the staging-flavor
        // history.db scanned the staging dir — sample rows linked to
        // audio paths that didn't exist for that flavor.
        guard let dir = DimmyCore.shared.configDirURL?
            .appendingPathComponent("history_audio", isDirectory: true) else { return nil }
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir
    }

    /// Write a 4-second bursty WAV to `url`. Returns the on-disk byte
    /// count or nil on any I/O failure (caller skips audio_path link).
    @discardableResult
    static func writeBurstyWAV(to url: URL) -> Int64? {
        let sampleRate: Double = 16_000
        let totalSecs: Double = 4.0
        let totalFrames = Int(totalSecs * sampleRate)

        // 4 syllable-like bursts. Pitch + duration vary so the waveform
        // bins render visibly different heights — boring monotone sine
        // would collapse the visual into a uniform rectangle.
        struct Burst { let startSec: Double; let durSec: Double; let hz: Double; let gain: Double }
        let bursts: [Burst] = [
            Burst(startSec: 0.20, durSec: 0.55, hz: 440, gain: 0.55),
            Burst(startSec: 1.05, durSec: 0.40, hz: 330, gain: 0.70),
            Burst(startSec: 1.80, durSec: 0.75, hz: 220, gain: 0.50),
            Burst(startSec: 3.00, durSec: 0.65, hz: 550, gain: 0.65),
        ]

        // Render to a float buffer first (AVAudioPCMBuffer wants float
        // channel data; AVAudioFile converts float → int16 on write
        // because we set the file format to LinearPCM 16-bit).
        var floatSamples = [Float](repeating: 0, count: totalFrames)
        for b in bursts {
            let startFrame = Int(b.startSec * sampleRate)
            let burstFrames = Int(b.durSec * sampleRate)
            for i in 0..<burstFrames {
                let frame = startFrame + i
                if frame >= totalFrames { break }
                let env = 0.5 - 0.5 * cos(2.0 * .pi * Double(i) / Double(burstFrames))
                let phase = 2.0 * .pi * b.hz * Double(i) / sampleRate
                let val = sin(phase) * env * b.gain
                floatSamples[frame] = Float(val)
            }
        }

        // File settings: 16-bit signed little-endian PCM mono 16 kHz.
        // Matches what dimmy_stop_recording produces, so the prune
        // thread treats it identically. AVAudioFile picks WAVE container
        // by url extension (.wav).
        let fileSettings: [String: Any] = [
            AVFormatIDKey: kAudioFormatLinearPCM,
            AVSampleRateKey: sampleRate,
            AVNumberOfChannelsKey: 1,
            AVLinearPCMBitDepthKey: 16,
            AVLinearPCMIsFloatKey: false,
            AVLinearPCMIsBigEndianKey: false,
            AVLinearPCMIsNonInterleaved: false,
        ]

        // Idempotent: replace any prior file at this path.
        if FileManager.default.fileExists(atPath: url.path) {
            try? FileManager.default.removeItem(at: url)
        }

        do {
            let file = try AVAudioFile(forWriting: url, settings: fileSettings)
            // The file's processing format is the canonical float
            // representation AVAudioFile uses internally. We allocate
            // the buffer in that format and copy our float samples in.
            guard let buffer = AVAudioPCMBuffer(
                pcmFormat: file.processingFormat,
                frameCapacity: AVAudioFrameCount(totalFrames)
            ) else { return nil }
            buffer.frameLength = AVAudioFrameCount(totalFrames)
            if let ch = buffer.floatChannelData {
                for i in 0..<totalFrames {
                    ch[0][i] = floatSamples[i]
                }
            }
            try file.write(from: buffer)
        } catch {
            print("[SampleAudioSynth] write failed: \(error)")
            return nil
        }

        let attrs = (try? FileManager.default.attributesOfItem(atPath: url.path)) ?? [:]
        return attrs[.size] as? Int64
    }
}

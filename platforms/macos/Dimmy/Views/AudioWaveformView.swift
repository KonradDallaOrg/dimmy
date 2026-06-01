import SwiftUI
import AppKit
import AVFoundation
import Accelerate

// MARK: - WaveformSamples
//
// Reads a WAV (or any AVAudioFile-supported format) and bins the
// sample magnitudes into N buckets for display. Bins are 0-1
// normalized peaks so the renderer doesn't have to know about
// sample-format / channel-count details. Decoded once per file
// because re-decoding 60s of 16 kHz mono on every redraw would
// burn CPU for no reason.

@MainActor
final class WaveformSamples: ObservableObject {
    @Published private(set) var bins: [Float] = []
    @Published private(set) var durationSecs: Double = 0
    @Published private(set) var loadFailed: Bool = false
    /// Number of horizontal bars the waveform is binned into. Tuned so
    /// the bars stay visually distinct on the typical 480-pt detail-
    /// sheet width while keeping decode cost low. `nonisolated` so the
    /// background decode pass can read it without a main-thread hop.
    nonisolated static let binCount = 200

    func load(path: String) {
        bins = []
        durationSecs = 0
        loadFailed = false
        guard !path.isEmpty else { loadFailed = true; return }
        let url = URL(fileURLWithPath: path)
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            guard let self else { return }
            let result = Self.decode(url: url)
            DispatchQueue.main.async {
                switch result {
                case .success(let payload):
                    self.bins = payload.bins
                    self.durationSecs = payload.duration
                case .failure:
                    self.loadFailed = true
                }
            }
        }
    }

    private struct Payload {
        let bins: [Float]
        let duration: Double
    }

    /// Pure decode, no shared state, safe to call from a background
    /// thread. Marked `nonisolated` so the @MainActor on the enclosing
    /// class doesn't require us to bounce off the main thread.
    nonisolated private static func decode(url: URL) -> Result<Payload, Error> {
        do {
            let file = try AVAudioFile(forReading: url)
            let frameCount = AVAudioFrameCount(file.length)
            guard frameCount > 0 else {
                return .failure(NSError(
                    domain: "WaveformSamples", code: 1,
                    userInfo: [NSLocalizedDescriptionKey: "empty file"]
                ))
            }
            let format = file.processingFormat
            guard let buffer = AVAudioPCMBuffer(
                pcmFormat: format,
                frameCapacity: frameCount
            ) else {
                return .failure(NSError(domain: "WaveformSamples", code: 2))
            }
            try file.read(into: buffer)
            let channels = Int(buffer.format.channelCount)
            let frames = Int(buffer.frameLength)
            guard frames > 0, let channelData = buffer.floatChannelData else {
                return .failure(NSError(domain: "WaveformSamples", code: 3))
            }

            // Bin into peak magnitudes. We average across channels first
            // (Dimmy histories are mono but this stays robust), then
            // take the max abs value per window for a clear waveform.
            let binCount = Self.binCount
            let samplesPerBin = max(1, frames / binCount)
            var bins = [Float](repeating: 0, count: binCount)

            for b in 0..<binCount {
                let start = b * samplesPerBin
                let end = min(start + samplesPerBin, frames)
                if start >= end { break }
                var peak: Float = 0
                for i in start..<end {
                    var mixed: Float = 0
                    for c in 0..<channels {
                        mixed += channelData[c][i]
                    }
                    mixed /= Float(channels)
                    let mag = abs(mixed)
                    if mag > peak { peak = mag }
                }
                bins[b] = peak
            }

            // Normalise to 0-1, the file's loudest peak becomes 1.0
            // so a quiet-but-clear recording reads visually the same
            // as a loud one. Whisper-grade denoised audio typically
            // peaks around 0.3 absolute, would otherwise look flat.
            if let maxPeak = bins.max(), maxPeak > 0 {
                let scale: Float = 1.0 / maxPeak
                vDSP_vsmul(bins, 1, [scale], &bins, 1, vDSP_Length(bins.count))
            }

            let duration = Double(frames) / format.sampleRate
            return .success(Payload(bins: bins, duration: duration))
        } catch {
            return .failure(error)
        }
    }
}

// MARK: - AudioWaveformView
//
// Visual waveform + transport. Click anywhere on the bars to seek;
// space (when focused) and the leading button toggle play/pause.
// Replaces the bare "Reveal in Finder" button on the previous detail
// sheet with the same playback affordance Win ships.

struct AudioWaveformView: View {
    let path: String
    var height: CGFloat = 56

    @StateObject private var samples = WaveformSamples()
    @State private var player: AVAudioPlayer?
    @State private var isPlaying: Bool = false
    @State private var currentTime: Double = 0
    @State private var pollTimer: Timer?

    var body: some View {
        VStack(spacing: 8) {
            HStack(spacing: 10) {
                Button {
                    togglePlayback()
                } label: {
                    Image(systemName: isPlaying ? "pause.fill" : "play.fill")
                        .font(.system(size: 14, weight: .semibold))
                        .foregroundStyle(.white)
                        .frame(width: 32, height: 32)
                        .background(
                            Circle()
                                .fill(LinearGradient(
                                    colors: [Color.accentColor, Color.accentColor.opacity(0.85)],
                                    startPoint: .top,
                                    endPoint: .bottom
                                ))
                                .shadow(color: Color.accentColor.opacity(0.45), radius: 6, y: 2)
                        )
                }
                .buttonStyle(.plain)
                .disabled(samples.loadFailed || samples.bins.isEmpty)

                waveform
                    .frame(height: height)
                    .frame(maxWidth: .infinity)

                Text(timeLabel)
                    .font(.system(size: 11, design: .monospaced))
                    .foregroundStyle(Color.macTextSecondary)
                    .monospacedDigit()
                    .frame(width: 80, alignment: .trailing)
            }
        }
        .onAppear {
            samples.load(path: path)
            preparePlayer()
        }
        .onDisappear {
            stopPolling()
            player?.stop()
        }
    }

    // MARK: Waveform render

    @ViewBuilder
    private var waveform: some View {
        if samples.loadFailed {
            HStack(spacing: 6) {
                Image(systemName: "exclamationmark.triangle.fill")
                    .foregroundStyle(.orange)
                Text("Couldn't decode audio").font(.system(size: 11))
                    .foregroundStyle(Color.macTextSecondary)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        } else if samples.bins.isEmpty {
            HStack(spacing: 8) {
                ProgressView().controlSize(.small).scaleEffect(0.7)
                Text("Loading waveform...")
                    .font(.system(size: 11))
                    .foregroundStyle(Color.macTextSecondary)
            }
        } else {
            GeometryReader { geo in
                let progress = samples.durationSecs > 0
                    ? CGFloat(currentTime / samples.durationSecs)
                    : 0
                Canvas { ctx, size in
                    let n = samples.bins.count
                    guard n > 0 else { return }
                    let barSpacing: CGFloat = 1
                    let barWidth = max(1, (size.width - CGFloat(n - 1) * barSpacing) / CGFloat(n))
                    let midY = size.height / 2
                    let played = progress * size.width
                    for (i, bin) in samples.bins.enumerated() {
                        let h = max(2, CGFloat(bin) * size.height * 0.95)
                        let x = CGFloat(i) * (barWidth + barSpacing)
                        let rect = CGRect(
                            x: x,
                            y: midY - h / 2,
                            width: barWidth,
                            height: h
                        )
                        let color: Color = (x + barWidth / 2) <= played
                            ? Color.accentColor
                            : Color.primary.opacity(0.32)
                        ctx.fill(
                            Path(roundedRect: rect, cornerRadius: barWidth / 2),
                            with: .color(color)
                        )
                    }
                    // Playhead vertical line.
                    if progress > 0 && progress < 1 {
                        let x = played
                        let line = Path(CGRect(x: x - 0.5, y: 0, width: 1, height: size.height))
                        ctx.fill(line, with: .color(Color.accentColor.opacity(0.9)))
                    }
                }
                .contentShape(Rectangle())
                .gesture(
                    DragGesture(minimumDistance: 0)
                        .onChanged { drag in
                            let frac = max(0, min(1, drag.location.x / geo.size.width))
                            seek(toFraction: Double(frac))
                        }
                )
            }
        }
    }

    // MARK: Playback

    private func preparePlayer() {
        guard !path.isEmpty,
              FileManager.default.fileExists(atPath: path)
        else { return }
        do {
            let p = try AVAudioPlayer(contentsOf: URL(fileURLWithPath: path))
            p.prepareToPlay()
            self.player = p
        } catch {
            // Decode error already surfaced via samples.loadFailed.
        }
    }

    private func togglePlayback() {
        guard let player = player else { return }
        if player.isPlaying {
            player.pause()
            isPlaying = false
            stopPolling()
        } else {
            // Loop back to start when finished so a second click replays.
            if player.currentTime >= player.duration - 0.05 {
                player.currentTime = 0
                currentTime = 0
            }
            player.play()
            isPlaying = true
            startPolling()
        }
    }

    private func seek(toFraction frac: Double) {
        guard let player = player else { return }
        let target = max(0, min(player.duration, frac * player.duration))
        player.currentTime = target
        currentTime = target
        if !isPlaying {
            // Drag-seek without auto-play feels right for a scrub.
        }
    }

    private func startPolling() {
        stopPolling()
        pollTimer = Timer.scheduledTimer(withTimeInterval: 0.05, repeats: true) { _ in
            Task { @MainActor in
                guard let p = player else { return }
                currentTime = p.currentTime
                if !p.isPlaying && isPlaying {
                    isPlaying = false
                    if p.currentTime >= p.duration - 0.05 {
                        currentTime = p.duration
                    }
                    stopPolling()
                }
            }
        }
    }

    private func stopPolling() {
        pollTimer?.invalidate()
        pollTimer = nil
    }

    private var timeLabel: String {
        let cur = formatTime(currentTime)
        let total = formatTime(samples.durationSecs)
        return "\(cur) / \(total)"
    }

    private func formatTime(_ secs: Double) -> String {
        guard secs.isFinite, secs >= 0 else { return "0:00" }
        let m = Int(secs) / 60
        let s = Int(secs) % 60
        return String(format: "%d:%02d", m, s)
    }
}

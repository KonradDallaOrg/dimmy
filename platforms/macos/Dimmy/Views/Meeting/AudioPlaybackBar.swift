import SwiftUI
import AppKit
import AVFoundation

// MARK: - AudioPlaybackBar
//
// Compact audio player + click/drag-to-seek waveform for the meeting
// Done view. Backs onto `AVAudioPlayer` (audio-only — AVKit's
// `VideoPlayer` cannot size audio-only assets and was the source of a
// SwiftUI layout-loop SIGABRT we hit earlier).
//
// The waveform replaces the thin Slider used in the first draft. Peaks
// are read once on load via `WavPeaks.readPeaks` (Mac port of
// `Helpers/WavPeaks.cs`). Each peak is one vertical bar drawn into a
// `Canvas`; the played portion shades to the accent colour, the
// unplayed portion stays in `macTextSecondary`. Clicking or dragging
// in the canvas seeks proportionally — the same gesture covers both
// "scrub" and "tap to jump" without a separate slider thumb.

struct AudioPlaybackBar: View {
    let url: URL
    let micURL: URL?
    let systemURL: URL?
    @StateObject private var model = AudioPlaybackModel()

    private let waveformBucketCount: Int = 120

    init(url: URL, micURL: URL? = nil, systemURL: URL? = nil) {
        self.url = url
        self.micURL = micURL
        self.systemURL = systemURL
    }

    private var dualBand: Bool {
        !model.peaksMic.isEmpty && !model.peaksSystem.isEmpty
    }

    private var progress: CGFloat {
        model.duration > 0 ? CGFloat(model.elapsed / model.duration) : 0
    }

    private func handleSeek(_ fraction: CGFloat) {
        guard model.duration > 0 else { return }
        model.seek(to: model.duration * Double(fraction))
    }

    var body: some View {
        HStack(spacing: 12) {
            Button(action: model.togglePlay) {
                Image(systemName: model.isPlaying ? "pause.fill" : "play.fill")
                    .font(.system(size: 14, weight: .semibold))
                    .frame(width: 28, height: 28)
            }
            .buttonStyle(.plain)
            .keyboardShortcut(.space, modifiers: [])
            .disabled(model.duration == 0)

            Text(model.format(model.elapsed))
                .font(.system(size: 11, design: .monospaced))
                .monospacedDigit()
                .foregroundStyle(Color.macTextSecondary)
                .frame(width: 44, alignment: .leading)

            Group {
                if dualBand {
                    DualBandWaveformStrip(
                        peaksMic: model.peaksMic,
                        peaksSystem: model.peaksSystem,
                        progress: progress,
                        onSeekFraction: handleSeek
                    )
                } else {
                    WaveformStrip(
                        peaks: model.peaks,
                        progress: progress,
                        onSeekFraction: handleSeek
                    )
                }
            }
            .frame(maxWidth: .infinity)
            .frame(height: 44)

            Text(model.format(model.duration))
                .font(.system(size: 11, design: .monospaced))
                .monospacedDigit()
                .foregroundStyle(Color.macTextSecondary)
                .frame(width: 44, alignment: .trailing)
        }
        .padding(.horizontal, 8)
        .onAppear {
            model.load(
                url: url,
                micURL: micURL,
                systemURL: systemURL,
                bucketCount: waveformBucketCount
            )
        }
        .onDisappear { model.stop() }
        .onChange(of: url) { _, newURL in
            model.load(
                url: newURL,
                micURL: micURL,
                systemURL: systemURL,
                bucketCount: waveformBucketCount
            )
        }
    }
}

// MARK: - Waveform strip

/// Canvas-based waveform with proportional click/drag seeking.
/// Renders one rounded-rect bar per bucket; the played portion is
/// tinted with the accent colour, the unplayed with `macTextSecondary`.
/// Bars centre-mirror around the vertical midpoint, like a typical
/// audio-editor scrub strip.
private struct WaveformStrip: View {
    let peaks: [Float]
    let progress: CGFloat
    let onSeekFraction: (CGFloat) -> Void

    var body: some View {
        GeometryReader { geo in
            ZStack(alignment: .leading) {
                Canvas { ctx, size in
                    drawBars(ctx: &ctx, size: size, gradient: unplayedGradient)
                }
                Canvas { ctx, size in
                    let clipWidth = size.width * max(0, min(1, progress))
                    ctx.clip(to: Path(CGRect(x: 0, y: 0, width: clipWidth, height: size.height)))
                    drawBars(ctx: &ctx, size: size, gradient: playedGradient)
                }
                // 1.5pt accent cursor with a soft glow so the playhead
                // pops even when the underlying waveform is near-silent.
                Rectangle()
                    .fill(Color.accentColor)
                    .frame(width: 1.5)
                    .shadow(color: Color.accentColor.opacity(0.6), radius: 3)
                    .offset(x: geo.size.width * max(0, min(1, progress)) - 0.75)
                    .opacity(progress > 0 ? 0.95 : 0)
            }
            .contentShape(Rectangle())
            .gesture(
                DragGesture(minimumDistance: 0)
                    .onChanged { value in
                        let f = max(0, min(1, value.location.x / geo.size.width))
                        onSeekFraction(f)
                    }
            )
        }
    }

    private var playedGradient: GraphicsContext.Shading {
        .linearGradient(
            Gradient(colors: [
                Color.accentColor,
                Color.accentColor.opacity(0.55),
            ]),
            startPoint: CGPoint(x: 0, y: 0),
            endPoint: CGPoint(x: 0, y: 1)
        )
    }

    private var unplayedGradient: GraphicsContext.Shading {
        .linearGradient(
            Gradient(colors: [
                Color.macTextSecondary.opacity(0.55),
                Color.macTextSecondary.opacity(0.25),
            ]),
            startPoint: CGPoint(x: 0, y: 0),
            endPoint: CGPoint(x: 0, y: 1)
        )
    }

    private func drawBars(ctx: inout GraphicsContext, size: CGSize, gradient: GraphicsContext.Shading) {
        guard !peaks.isEmpty else {
            // Empty / unparsable waveform — render a thin baseline so
            // the strip doesn't disappear visually.
            let mid = size.height / 2
            let path = Path { p in
                p.move(to: CGPoint(x: 0, y: mid))
                p.addLine(to: CGPoint(x: size.width, y: mid))
            }
            ctx.stroke(path, with: gradient, lineWidth: 1)
            return
        }
        let n = peaks.count
        let totalWidth = size.width
        let gap: CGFloat = 2
        // Wider bars + 2pt gap (chunkier than the original 1pt gap with
        // 220 buckets) — gives the waveform real visual weight at the
        // ~120 bucket count used here.
        let barWidth = max(2, (totalWidth - CGFloat(n - 1) * gap) / CGFloat(n))
        let mid = size.height / 2
        let maxHalf = size.height / 2 - 2  // 2pt vertical padding
        let cornerRadius: CGFloat = 2
        for (i, peak) in peaks.enumerated() {
            let x = CGFloat(i) * (barWidth + gap)
            let half = max(1.5, CGFloat(peak) * maxHalf)
            let rect = CGRect(x: x, y: mid - half, width: barWidth, height: half * 2)
            ctx.fill(Path(roundedRect: rect, cornerRadius: cornerRadius, style: .continuous),
                      with: gradient)
        }
    }
}

// MARK: - Dual-band waveform strip
//
// Mirror of Win MeetingWindow.xaml.cs:841-910 — mic peaks anchored on
// the shared midline grow UP (DodgerBlue), system peaks grow DOWN
// (LimeGreen). A single accent playhead sweeps both bands. Click /
// drag seeks the underlying AVAudioPlayer through `onSeekFraction`.
//
// Falls back to a thin baseline if either band has zero samples, but
// the caller (`AudioPlaybackBar`) only routes here when both arrays
// are non-empty.

private struct DualBandWaveformStrip: View {
    let peaksMic: [Float]
    let peaksSystem: [Float]
    let progress: CGFloat
    let onSeekFraction: (CGFloat) -> Void

    private static let micColor = Color(red: 0.118, green: 0.565, blue: 1.000)        // DodgerBlue
    private static let systemColor = Color(red: 0.196, green: 0.804, blue: 0.196)     // LimeGreen

    var body: some View {
        GeometryReader { geo in
            ZStack(alignment: .leading) {
                Canvas { ctx, size in
                    drawBand(ctx: &ctx, size: size, peaks: peaksMic,
                             color: Self.micColor.opacity(0.45), direction: .up)
                    drawBand(ctx: &ctx, size: size, peaks: peaksSystem,
                             color: Self.systemColor.opacity(0.45), direction: .down)
                }
                Canvas { ctx, size in
                    let clipWidth = size.width * max(0, min(1, progress))
                    ctx.clip(to: Path(CGRect(x: 0, y: 0, width: clipWidth, height: size.height)))
                    drawBand(ctx: &ctx, size: size, peaks: peaksMic,
                             color: Self.micColor, direction: .up)
                    drawBand(ctx: &ctx, size: size, peaks: peaksSystem,
                             color: Self.systemColor, direction: .down)
                }
                // Centre baseline so the empty stretches at start/end
                // don't look broken — and so the two bands feel like
                // one continuous strip.
                Rectangle()
                    .fill(Color.macTextSecondary.opacity(0.25))
                    .frame(height: 0.5)
                    .offset(y: 0)
                Rectangle()
                    .fill(Color.accentColor)
                    .frame(width: 1.5)
                    .shadow(color: Color.accentColor.opacity(0.6), radius: 3)
                    .offset(x: geo.size.width * max(0, min(1, progress)) - 0.75)
                    .opacity(progress > 0 ? 0.95 : 0)
            }
            .contentShape(Rectangle())
            .gesture(
                DragGesture(minimumDistance: 0)
                    .onChanged { value in
                        let f = max(0, min(1, value.location.x / geo.size.width))
                        onSeekFraction(f)
                    }
            )
        }
    }

    private enum BandDirection { case up, down }

    private func drawBand(ctx: inout GraphicsContext,
                          size: CGSize,
                          peaks: [Float],
                          color: Color,
                          direction: BandDirection) {
        guard !peaks.isEmpty else { return }
        let n = peaks.count
        let gap: CGFloat = 2
        let barWidth = max(2, (size.width - CGFloat(n - 1) * gap) / CGFloat(n))
        let mid = size.height / 2
        let maxReach = max(2, size.height / 2 - 2)
        let cornerRadius: CGFloat = 2
        let shading = GraphicsContext.Shading.color(color)
        for (i, peak) in peaks.enumerated() {
            let x = CGFloat(i) * (barWidth + gap)
            let h = max(1.0, CGFloat(peak) * maxReach)
            let rect: CGRect
            switch direction {
            case .up:
                rect = CGRect(x: x, y: mid - h, width: barWidth, height: h)
            case .down:
                rect = CGRect(x: x, y: mid, width: barWidth, height: h)
            }
            ctx.fill(Path(roundedRect: rect, cornerRadius: cornerRadius, style: .continuous),
                     with: shading)
        }
    }
}

// MARK: - Model

@MainActor
final class AudioPlaybackModel: ObservableObject {
    @Published var elapsed: TimeInterval = 0
    @Published var duration: TimeInterval = 0
    @Published var isPlaying: Bool = false
    @Published var peaks: [Float] = []
    /// Per-track mic peaks (read from `audio_mic.wav`). Empty when the
    /// caller didn't pass `micURL` or the file is missing.
    @Published var peaksMic: [Float] = []
    /// Per-track system peaks (read from `audio_system.wav`).
    @Published var peaksSystem: [Float] = []

    private var player: AVAudioPlayer?
    private var timer: Timer?
    private var lastUrl: URL?
    /// Tmp WAV produced by `dimmy_decode_audio_to_wav` when the mix
    /// track on disk is Ogg/Vorbis (AVAudioPlayer doesn't decode Ogg
    /// natively on macOS). Cleared in `stop()` so the tmp directory
    /// doesn't accumulate one-shot decodes meeting after meeting.
    private var tempDecodedWavURL: URL?

    func load(url: URL, micURL: URL? = nil, systemURL: URL? = nil, bucketCount: Int) {
        guard url != lastUrl else { return }
        stop()
        lastUrl = url
        // Pick the URL we'll hand to AVAudioPlayer. For .ogg meetings
        // (Mac after the gate flip) decode to a tmp WAV first; for .wav
        // (older meetings + file-load) use the URL directly. The peaks
        // path keeps the ORIGINAL URL — `WavPeaks.readPeaks` already
        // routes .ogg through the FFI Symphonia decoder, so the waveform
        // is correct independent of the playback codec.
        let playbackURL: URL
        if url.pathExtension.lowercased() == "ogg" {
            let tmp = FileManager.default.temporaryDirectory
                .appendingPathComponent("dimmy_ogg_play_\(UUID().uuidString).wav")
            if DimmyCore.shared.decodeAudioToWav(source: url.path, destination: tmp.path) {
                tempDecodedWavURL = tmp
                playbackURL = tmp
            } else {
                // Decode failed → no playback, but waveform + transcript
                // still work (the doc handover accepts this fallback).
                playbackURL = url
            }
        } else {
            playbackURL = url
        }
        do {
            let p = try AVAudioPlayer(contentsOf: playbackURL)
            p.prepareToPlay()
            self.player = p
            self.duration = p.duration
            self.elapsed = 0
        } catch {
            self.player = nil
            self.duration = 0
            self.elapsed = 0
        }
        // Compute peaks on a background queue — a 60s 16-bit mono WAV
        // is ~6 MB, parses in <50 ms, but we don't want to block the
        // first paint of the Done view on it.
        let path = url.path
        let micPath = micURL?.path
        let systemPath = systemURL?.path
        let n = bucketCount
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            let mix = WavPeaks.readPeaks(path: path, bucketCount: n)
            let mic = micPath.map { WavPeaks.readPeaks(path: $0, bucketCount: n) } ?? []
            let sys = systemPath.map { WavPeaks.readPeaks(path: $0, bucketCount: n) } ?? []
            DispatchQueue.main.async {
                guard let self, self.lastUrl == url else { return }
                self.peaks = mix
                self.peaksMic = mic
                self.peaksSystem = sys
            }
        }
    }

    func togglePlay() {
        guard let player else { return }
        if player.isPlaying {
            player.pause()
            isPlaying = false
            stopTimer()
        } else {
            player.play()
            isPlaying = true
            startTimer()
        }
    }

    func seek(to t: TimeInterval) {
        guard let player else { return }
        player.currentTime = max(0, min(t, player.duration))
        elapsed = player.currentTime
    }

    func stop() {
        player?.stop()
        player = nil
        stopTimer()
        isPlaying = false
        elapsed = 0
        duration = 0
        peaks = []
        peaksMic = []
        peaksSystem = []
        lastUrl = nil
        if let tmp = tempDecodedWavURL {
            try? FileManager.default.removeItem(at: tmp)
            tempDecodedWavURL = nil
        }
    }

    private func startTimer() {
        stopTimer()
        timer = Timer.scheduledTimer(withTimeInterval: 0.05, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.tick() }
        }
    }

    private func stopTimer() {
        timer?.invalidate()
        timer = nil
    }

    private func tick() {
        guard let player else { return }
        elapsed = player.currentTime
        if !player.isPlaying, isPlaying {
            isPlaying = false
            stopTimer()
        }
    }

    func format(_ t: TimeInterval) -> String {
        let total = max(0, Int(t.rounded()))
        return String(format: "%02d:%02d", total / 60, total % 60)
    }
}

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
    @StateObject private var model = AudioPlaybackModel()

    private let waveformBucketCount: Int = 120

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

            WaveformStrip(
                peaks: model.peaks,
                progress: model.duration > 0
                    ? CGFloat(model.elapsed / model.duration)
                    : 0,
                onSeekFraction: { fraction in
                    guard model.duration > 0 else { return }
                    model.seek(to: model.duration * Double(fraction))
                }
            )
            .frame(maxWidth: .infinity)
            .frame(height: 44)

            Text(model.format(model.duration))
                .font(.system(size: 11, design: .monospaced))
                .monospacedDigit()
                .foregroundStyle(Color.macTextSecondary)
                .frame(width: 44, alignment: .trailing)
        }
        .padding(.horizontal, 8)
        .onAppear { model.load(url: url, bucketCount: waveformBucketCount) }
        .onDisappear { model.stop() }
        .onChange(of: url) { _, newURL in
            model.load(url: newURL, bucketCount: waveformBucketCount)
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

// MARK: - Model

@MainActor
final class AudioPlaybackModel: ObservableObject {
    @Published var elapsed: TimeInterval = 0
    @Published var duration: TimeInterval = 0
    @Published var isPlaying: Bool = false
    @Published var peaks: [Float] = []

    private var player: AVAudioPlayer?
    private var timer: Timer?
    private var lastUrl: URL?

    func load(url: URL, bucketCount: Int) {
        guard url != lastUrl else { return }
        stop()
        lastUrl = url
        do {
            let p = try AVAudioPlayer(contentsOf: url)
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
        let n = bucketCount
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            let read = WavPeaks.readPeaks(path: path, bucketCount: n)
            DispatchQueue.main.async {
                guard let self, self.lastUrl == url else { return }
                self.peaks = read
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
        lastUrl = nil
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

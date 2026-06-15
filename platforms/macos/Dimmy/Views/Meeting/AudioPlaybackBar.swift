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

    // Decode a generous bucket count; the strips resample this down to the
    // width-adaptive slim-bar count at draw time (so any duration fills the
    // card). Mirrors Win's 400-bucket decode.
    private let waveformBucketCount: Int = 400

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
// Brand gradient (logo): green #2ECE8E (top / mic) → violet #6E7DF7 (bottom /
// system). Slim dense bars (3pt + 2pt gap), width-adaptive count.
private let waveGreen = Color(red: 46.0 / 255, green: 206.0 / 255, blue: 142.0 / 255)
private let waveViolet = Color(red: 110.0 / 255, green: 125.0 / 255, blue: 247.0 / 255)
private let waveBarW: CGFloat = 3
private let waveBarGap: CGFloat = 2

/// Vertical green→violet gradient mapped to the canvas height, so a bar near
/// the top reads green and one near the bottom reads violet. `fade` = the
/// faded "unplayed" version.
private func waveShading(height: CGFloat, fade: Bool) -> GraphicsContext.Shading {
    let colors = fade
        ? [waveGreen.opacity(0.30), waveViolet.opacity(0.30)]
        : [waveGreen, waveViolet]
    return .linearGradient(
        Gradient(colors: colors),
        startPoint: CGPoint(x: 0, y: 0),
        endPoint: CGPoint(x: 0, y: height))
}

/// Resample peaks to exactly `n` bars by max (spikes survive). Fills the card
/// at any duration: long audio = peak over a longer slice ⇒ fuller/smoother.
private func waveResampleMax(_ src: [Float], _ n: Int) -> [Float] {
    guard n > 0 else { return [] }
    if src.isEmpty { return Array(repeating: 0, count: n) }
    var out = [Float](repeating: 0, count: n)
    let step = Double(src.count) / Double(n)
    for i in 0..<n {
        let a = Int(Double(i) * step)
        var b = Int(Double(i + 1) * step)
        if b <= a { b = a + 1 }
        if b > src.count { b = src.count }
        var mx: Float = 0
        var j = a
        while j < b { if src[j] > mx { mx = src[j] }; j += 1 }
        out[i] = mx
    }
    return out
}

/// Normalise so the loudest bar ~fills the half-height (quiet recordings still
/// look full) without clipping a loud one.
private func waveNorm(_ bars: [Float]) -> Double {
    var mx: Float = 0.05
    for v in bars where v > mx { mx = v }
    return min(1.0 / Double(mx), 3.2) * 0.92
}

/// Circular scrubber knob (white fill, violet ring + dot) with a faint guide
/// line. Replaces the old thin accent cursor.
private struct WaveKnob: View {
    let progress: CGFloat
    let width: CGFloat
    let height: CGFloat
    var body: some View {
        let cx = max(7, min(width - 7, width * max(0, min(1, progress))))
        ZStack {
            Rectangle()
                .fill(Color.macTextSecondary.opacity(0.35))
                .frame(width: 1.5, height: max(0, height - 12))
            Circle()
                .fill(Color.white)
                .overlay(Circle().stroke(waveViolet, lineWidth: 2.2))
                .overlay(Circle().fill(waveViolet).frame(width: 5, height: 5))
                .frame(width: 14, height: 14)
        }
        .offset(x: cx - 7)
    }
}

private struct WaveformStrip: View {
    let peaks: [Float]
    let progress: CGFloat
    let onSeekFraction: (CGFloat) -> Void

    var body: some View {
        GeometryReader { geo in
            ZStack(alignment: .leading) {
                Canvas { ctx, size in drawBars(ctx: &ctx, size: size, fade: true) }
                Canvas { ctx, size in
                    let clipWidth = size.width * max(0, min(1, progress))
                    ctx.clip(to: Path(CGRect(x: 0, y: 0, width: clipWidth, height: size.height)))
                    drawBars(ctx: &ctx, size: size, fade: false)
                }
                WaveKnob(progress: progress, width: geo.size.width, height: geo.size.height)
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

    private func drawBars(ctx: inout GraphicsContext, size: CGSize, fade: Bool) {
        guard !peaks.isEmpty else { return }
        let slot = waveBarW + waveBarGap
        let n = max(1, Int(size.width / slot))
        let bars = waveResampleMax(peaks, n)
        let norm = waveNorm(bars)
        let mid = size.height / 2
        let maxHalf = size.height / 2 - 3
        let shading = waveShading(height: size.height, fade: fade)
        for (i, peak) in bars.enumerated() {
            let x = CGFloat(i) * slot
            let half = max(1.5, CGFloat(Double(peak) * norm) * maxHalf)
            let rect = CGRect(x: x, y: mid - half, width: waveBarW, height: half * 2)
            ctx.fill(Path(roundedRect: rect, cornerRadius: waveBarW / 2, style: .continuous), with: shading)
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

    var body: some View {
        GeometryReader { geo in
            ZStack(alignment: .leading) {
                Canvas { ctx, size in drawDual(ctx: &ctx, size: size, fade: true) }
                Canvas { ctx, size in
                    let clipWidth = size.width * max(0, min(1, progress))
                    ctx.clip(to: Path(CGRect(x: 0, y: 0, width: clipWidth, height: size.height)))
                    drawDual(ctx: &ctx, size: size, fade: false)
                }
                // Faint centre hairline so silent stretches at start/end
                // read as one continuous strip.
                Rectangle()
                    .fill(Color.macTextSecondary.opacity(0.22))
                    .frame(height: 0.5)
                WaveKnob(progress: progress, width: geo.size.width, height: geo.size.height)
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

    /// mic grows UP from the midline, system grows DOWN. Both filled with the
    /// shared vertical green→violet gradient (so the band itself carries the
    /// colour: greener at top, more violet at bottom).
    private func drawDual(ctx: inout GraphicsContext, size: CGSize, fade: Bool) {
        let slot = waveBarW + waveBarGap
        let n = max(1, Int(size.width / slot))
        let m = waveResampleMax(peaksMic, n)
        let s = waveResampleMax(peaksSystem, n)
        let norm = waveNorm(m + s)
        let mid = size.height / 2
        let maxReach = max(2, size.height / 2 - 3)
        let shading = waveShading(height: size.height, fade: fade)
        for i in 0..<n {
            let x = CGFloat(i) * slot
            let hm = max(1.5, CGFloat(Double(m[i]) * norm) * maxReach)
            let hs = max(1.5, CGFloat(Double(s[i]) * norm) * maxReach)
            ctx.fill(Path(roundedRect: CGRect(x: x, y: mid - hm, width: waveBarW, height: hm),
                          cornerRadius: waveBarW / 2, style: .continuous), with: shading)
            ctx.fill(Path(roundedRect: CGRect(x: x, y: mid, width: waveBarW, height: hs),
                          cornerRadius: waveBarW / 2, style: .continuous), with: shading)
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

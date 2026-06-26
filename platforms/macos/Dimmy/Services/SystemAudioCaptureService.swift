import Foundation
import ScreenCaptureKit
import AVFoundation

/// Captures system audio for meeting mode and forwards f32 PCM samples to
/// Rust via `dimmy_push_loopback_audio`.
///
/// Two backends, picked at `start()`:
///   • **Core Audio process tap** (macOS 14.4+, preferred) — records other
///     processes' audio output with only the audio-recording TCC grant (the
///     discreet purple dot). No Screen Recording prompt, no Sequoia media-
///     library prompt. See `SystemAudioProcessTap`.
///   • **ScreenCaptureKit** (macOS 14.0–14.3, or if the tap fails) — the
///     legacy `SCStream` loopback path, kept as a safety net so the older
///     OSes don't regress. This is the only path that needs Screen
///     Recording, and it only runs when the tap is unavailable.
///
/// Lifecycle: call start() after dimmy_meeting_start; stop() before/after
/// dimmy_meeting_stop. If neither backend can start (permission denied),
/// start() returns false and the meeting continues mic-only.
@MainActor
final class SystemAudioCaptureService: NSObject {
    static let shared = SystemAudioCaptureService()
    private var stream: SCStream?
    private var isRunning = false
    /// Active Core Audio tap (macOS 14.4+). Stored as AnyObject because the
    /// concrete type is gated above the 14.0 deployment target.
    private var processTap: AnyObject?
    private override init() {}

    /// Whether the system-audio path is healthy. Three cases for the tap
    /// path:
    ///
    /// - IO proc has fired (`hasReceivedAudio == true`) → audio is flowing,
    ///   return true.
    /// - Tap is in deferred state (`currentTapPidSet.isEmpty`, i.e. no audio
    ///   source was active at start and the rescan timer hasn't promoted
    ///   anything yet) → there's nothing to capture *and* no TCC denial
    ///   to flag; return true so the meeting permission banner doesn't
    ///   falsely fire when the user starts recording before opening their
    ///   videoconf app.
    /// - Tap has sources (`currentTapPidSet` non-empty) but IO proc has
    ///   not fired → likely missing AudioCapture grant; return false so
    ///   the banner can warn.
    ///
    /// The SCStream path returns true once started (its own start()
    /// already fails closed on denial).
    var isCapturingSystemAudio: Bool {
        if #available(macOS 14.4, *), let tap = processTap as? SystemAudioProcessTap {
            if tap.hasReceivedAudio { return true }
            return tap.currentTapPidSet.isEmpty
        }
        return stream != nil
    }

    /// Mirrors the cpal mic rate when one is live; falls back through
    /// a cpal probe (no stream opened) and finally to 48 kHz.
    ///
    /// Resolution order:
    ///   1. `dimmy_get_active_mic_device_rate()` — device-native rate
    ///      of the live cpal stream (e.g. 16 kHz on BT-HFP). This is
    ///      the wire rate SCStream must match to avoid SCKit either
    ///      upsampling internally or forcing HAL renegotiation.
    ///   2. `dimmy_get_active_mic_sample_rate()` — canonical
    ///      post-resample rate (always 48 kHz when live). Fallback
    ///      while the device-rate atomic is being populated.
    ///   3. `dimmy_probe_primary_sample_rate()` — what cpal would open
    ///      the selected mic at; closes the race where SCStream starts
    ///      before the cpal Start command has been serviced.
    ///   4. Hardcoded 48 000 Hz when no input device is available
    ///      (SCStream still needs some rate).
    static func plannedSampleRate() -> Int {
        // Device-native rate first (e.g. 16 kHz on BT-HFP). Falls back to
        // the canonical post-resample rate, then to the cpal probe, then
        // to a hardcoded 48 kHz when no input device is configured.
        let device = Int(dimmy_get_active_mic_device_rate())
        if device > 0 { return device }
        let active = Int(dimmy_get_active_mic_sample_rate())
        if active > 0 { return active }
        let probed = Int(dimmy_probe_primary_sample_rate())
        if probed > 0 { return probed }
        return 48_000
    }

    /// UserDefaults key that pins the (appVersion, osMajor) tuple in which
    /// the Core Audio process tap was found silent. When the current launch
    /// matches that tuple, we skip the tap entirely and go straight to
    /// ScreenCaptureKit — avoids the ~800 ms dead window every meeting on
    /// Tahoe ad-hoc builds where the tap is structurally broken.
    ///
    /// The combo resets implicitly when either field changes:
    ///   • app upgrade (e.g. Developer-ID-signed release lands) → version
    ///     bumps → cache miss → tap is re-attempted on the next meeting.
    ///   • macOS upgrade → osMajor bumps → cache miss → re-attempted.
    /// We also explicitly clear it whenever the tap *does* deliver audio,
    /// so a transient denial (e.g. user toggled a permission mid-session)
    /// doesn't keep us pinned to SCKit forever.
    private static let tapSilentEnvDefaultsKey = "DimmySystemAudioTapSilentEnv"

    private static func currentTapEnvFingerprint() -> String {
        let version = Bundle.main.infoDictionary?["CFBundleShortVersionString"]
            as? String ?? "unknown"
        let os = ProcessInfo.processInfo.operatingSystemVersion
        return "\(version):\(os.majorVersion).\(os.minorVersion)"
    }

    func start() async -> Bool {
        guard !isRunning else { return true }

        // Prefer the Core Audio process tap — audio-only permission, no
        // screen-recording / media-library prompts. Falls through to
        // ScreenCaptureKit on:
        //   1. older OS / no default output / HAL refusal — `tap.start()` returns false
        //   2. macOS 26 (Tahoe) on ad-hoc signed bundles — `tap.start()` returns true
        //      (HAL accepts every call) but the IO proc never fires because tccd
        //      silently denies `kTCCServiceAudioCapture` to non-Developer-ID apps.
        //      Detect this by waiting briefly for the first frame; if nothing
        //      arrives, record the (appVersion, osMajor) combo in UserDefaults
        //      so subsequent meetings skip the tap immediately and start SCKit
        //      with zero dead-air.
        let currentEnv = Self.currentTapEnvFingerprint()
        let cachedSilentEnv = UserDefaults.standard.string(
            forKey: Self.tapSilentEnvDefaultsKey)
        let skipTapDueToCache = cachedSilentEnv == currentEnv

        if !skipTapDueToCache, #available(macOS 14.4, *) {
            let tap = SystemAudioProcessTap()
            tap.onSamples = { ptr, count, rate in
                _ = dimmy_push_loopback_audio(ptr, Int32(count), rate)
            }
            // Per-process tap StartOutcome dispatch:
            //   .live     → tap is recording, keep it
            //   .deferred → no audio source yet; rescan timer will promote
            //               the moment one appears (~3 s latency). Keep
            //               the tap instance — falling through to SCKit
            //               here would defeat the per-process design.
            //   .failed   → HAL error or denied grant. Cache the env so
            //               next meetings skip the tap instantly, fall
            //               through to SCStream.
            switch tap.start() {
            case .live:
                processTap = tap
                isRunning = true
                // Tahoe (macOS 26) silent-deny watchdog: `.live` only means
                // AudioDeviceStart returned noErr — NOT that the IO proc ever
                // fires. On non-Developer-ID bundles tccd accepts every call
                // but never delivers a frame, leaving the meeting permanently
                // mic-only with NO fallback (burned 2026-06-26, colleague's
                // stable build on 26.2). Verify a real frame lands within a
                // short window; if not, treat it like `.failed`: cache the
                // silent env and fall through to ScreenCaptureKit (which works
                // with the Screen Recording grant).
                try? await Task.sleep(nanoseconds: 1_500_000_000)
                if tap.hasReceivedAudio {
                    UserDefaults.standard.removeObject(
                        forKey: Self.tapSilentEnvDefaultsKey)
                    NSLog("[SystemAudio] capture via Core Audio process tap (frames confirmed)")
                    return true
                }
                NSLog("[SystemAudio] tap live but 0 frames in 1.5s (Tahoe silent-deny) — cached env=%@ -> ScreenCaptureKit fallback",
                    currentEnv)
                tap.stop()
                processTap = nil
                isRunning = false
                UserDefaults.standard.set(
                    currentEnv, forKey: Self.tapSilentEnvDefaultsKey)
                // fall through to startWithScreenCapture() below
            case .deferred:
                processTap = tap
                isRunning = true
                UserDefaults.standard.removeObject(
                    forKey: Self.tapSilentEnvDefaultsKey)
                NSLog("[SystemAudio] tap deferred (no audio source yet) — rescan will promote when audio appears")
                return true
            case .failed:
                UserDefaults.standard.set(
                    currentEnv, forKey: Self.tapSilentEnvDefaultsKey)
                NSLog("[SystemAudio] tap creation failed — cached env=%@ → ScreenCaptureKit fallback",
                    currentEnv)
            }
        } else if skipTapDueToCache {
            NSLog("[SystemAudio] skipping process tap (env=%@ previously silent) → ScreenCaptureKit fallback",
                currentEnv)
        }

        return await startWithScreenCapture()
    }

    /// ScreenCaptureKit fallback (macOS 14.0–14.3 / tap failure). Triggers
    /// the Screen Recording grant — only reached when the tap can't run.
    private func startWithScreenCapture() async -> Bool {
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
        if #available(macOS 14.4, *), let tap = processTap as? SystemAudioProcessTap {
            tap.stop()
            processTap = nil
            isRunning = false
            return
        }
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

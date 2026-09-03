import AppKit
import AudioToolbox
import CoreAudio
import Foundation
import os

/// Derives the tap's TRUE delivery rate from the hardware timestamps
/// CoreAudio hands the IO proc, instead of trusting the rate the aggregate's
/// format DECLARES.
///
/// On an aggregate device the nominal sample rate is a *request*, not a
/// fact. With a Bluetooth-HFP headset as default output the sub-device
/// clocks at 16 kHz while `readTapFormat` keeps reporting the nominal
/// 48 kHz. The core trusted that claim, saw src == canonical, took the
/// identity path (no resampler at all) and wrote `audio_system.wav` 3x fast
/// while the meeting worker zero-filled the 2/3 shortfall — "veloce e a
/// tratti", participants never transcribed (colleague's Mac, 2026-07-21).
///
/// Pinning the aggregate to 48 kHz makes the claim true and stays, but it is
/// one argument against the HAL with nothing behind it. This measures:
/// frames delivered over `mHostTime` (mach ticks, the machine's own clock)
/// *is* the delivery rate, by definition. It settles in ~250 ms.
///
/// Pure arithmetic on purpose — `ticksPerSecond` is injected so the whole
/// decision is unit-testable without CoreAudio or a Mac.
struct LoopbackRateEstimator: Sendable {
    /// Rates a real endpoint can clock at. Mirrors the Rust
    /// `STANDARD_LOOPBACK_RATES` so both ends snap identically.
    static let standardRates: [Int32] = [
        8_000, 11_025, 16_000, 22_050, 24_000, 32_000, 44_100, 48_000,
    ]

    /// Observation window before the FIRST verdict. ~250 ms is ~12 callbacks
    /// at the 20 ms cycle an HFP aggregate runs, and 16x faster than the
    /// wall-clock canary in the Rust core it supersedes.
    static let minObservationSeconds = 0.25
    /// Window length once a verdict stands. Longer, and a revision needs
    /// `revisionsRequired` consecutive windows agreeing — see `settled`.
    static let revisionObservationSeconds = 1.0
    static let revisionsRequired = 2
    static let minCallbacks = 8
    /// The HAL can dump a startup backlog on the opening callbacks, which
    /// reads as a rate spike. A spike snaps AWAY from a lower true rate, so
    /// it could only ever make us keep the claim — never adopt a wrong low
    /// one — but there is no reason to feed it in.
    static let warmupCallbacksSkipped = 2

    /// Nearest standard rate to a raw measurement.
    static func snap(_ hz: Double) -> Int32 {
        var best = standardRates[0]
        var bestDelta = Double.infinity
        for rate in standardRates {
            let delta = abs(Double(rate) - hz)
            if delta < bestDelta {
                bestDelta = delta
                best = rate
            }
        }
        return best
    }

    /// Nearest standard rate, but only when the measurement actually sits
    /// close to it (within 12 %). A reading in no-man's-land is noise, not a
    /// rate — return nil and keep watching rather than latch a guess.
    static func standardRate(forMeasured hz: Double) -> Int32? {
        guard hz > 0 else { return nil }
        let snapped = snap(hz)
        return abs(hz - Double(snapped)) <= 0.12 * Double(snapped) ? snapped : nil
    }

    private var callbacks = 0
    private var windowStart: UInt64?
    private var windowCallbacks = 0
    private var accumulatedFrames = 0.0
    /// Snapshot at the window's midpoint, so the two halves can be compared
    /// at close. See `isConsistent` — a window straddling a rate change
    /// averages both phases and must not be believed.
    private var halfFrames: Double?
    private var halfElapsed: Double?
    private var pendingRate: Int32?
    private var pendingWindows = 0

    private mutating func resetWindow(at hostTime: UInt64) {
        windowStart = hostTime
        windowCallbacks = 0
        accumulatedFrames = 0
        halfFrames = nil
        halfElapsed = nil
    }

    /// Did this window measure ONE rate, or the average of two?
    ///
    /// A window spanning a Bluetooth profile flip sees half its frames at
    /// 48 kHz and half at 16 kHz, which averages to ~32 kHz — itself a
    /// standard rate, so the proximity filter cannot reject it, and two such
    /// windows in a row would install a rate the hardware never ran at.
    /// Demanding both halves agree with the whole turns an inconsistent
    /// window into a non-measurement instead of a wrong one.
    private static func isConsistent(
        whole: Int32, totalFrames: Double, totalElapsed: Double,
        halfFrames: Double?, halfElapsed: Double?
    ) -> Bool {
        guard let halfFrames, let halfElapsed,
            halfElapsed > 0, totalElapsed > halfElapsed
        else {
            // No midpoint snapshot (window closed in one step): nothing to
            // cross-check, so take the whole-window reading at face value.
            return true
        }
        let first = standardRate(forMeasured: halfFrames / halfElapsed)
        let second = standardRate(
            forMeasured: (totalFrames - halfFrames) / (totalElapsed - halfElapsed))
        return first == whole && second == whole
    }

    /// The standing verdict. Heavily damped, but NOT frozen.
    ///
    /// Frozen was the first design, on the grounds that rebuilding the
    /// downstream resampler mid-stream tears up its phase state — the reason
    /// the July reactive-override attempt was reverted as "right sometimes,
    /// wrong on bursty delivery, flips in the middle". Freezing turned out to
    /// be wrong for the single most common case on this hardware: a Bluetooth
    /// headset keeps the SAME device UID while its profile flips A2DP 48 kHz →
    /// HFP 16 kHz the moment a mic opens. `shouldRebuildForOutputChange`
    /// compares UIDs, so nothing rebuilds and a frozen verdict would stay
    /// wrong for the rest of the meeting.
    ///
    /// So: revise, but only on `revisionsRequired` CONSECUTIVE full windows
    /// of `revisionObservationSeconds` that all agree on a different standard
    /// rate. Two seconds of consistent hardware-clock evidence is not
    /// "bursty delivery"; one click at the transition beats an hour of audio
    /// at the wrong speed.
    private(set) var settled: Int32?

    /// Feed one IO-proc callback. Returns the rate to use and whether THIS
    /// call changed it, so the caller can log each transition exactly once.
    /// Nil only while no verdict has ever been reached.
    mutating func observe(
        deliveredFrames: Int,
        hostTime: UInt64,
        ticksPerSecond: Double
    ) -> (rate: Int32, isNew: Bool)? {
        // Every early return reports `standing`, the verdict already reached:
        // a silent or degenerate callback is not evidence, but it does not
        // un-know what we already measured. Nil only before the first verdict.
        let standing: (rate: Int32, isNew: Bool)? = settled.map { ($0, false) }

        guard deliveredFrames > 0, ticksPerSecond > 0 else { return standing }

        callbacks += 1
        guard callbacks > Self.warmupCallbacksSkipped else { return standing }

        guard let start = windowStart else {
            // Frames of THIS callback were delivered before the window
            // opened, so they are not ours to count.
            windowStart = hostTime
            return standing
        }
        accumulatedFrames += Double(deliveredFrames)
        windowCallbacks += 1

        let elapsed = Double(hostTime &- start) / ticksPerSecond
        let required =
            settled == nil ? Self.minObservationSeconds : Self.revisionObservationSeconds

        if halfFrames == nil, elapsed >= required / 2 {
            halfFrames = accumulatedFrames
            halfElapsed = elapsed
        }

        guard elapsed >= required, windowCallbacks >= Self.minCallbacks else {
            return standing
        }

        let totalFrames = accumulatedFrames
        let firstHalfFrames = halfFrames
        let firstHalfElapsed = halfElapsed
        resetWindow(at: hostTime)

        let candidate = Self.standardRate(forMeasured: totalFrames / elapsed)
        let rateOrNil = candidate.flatMap { rate in
            Self.isConsistent(
                whole: rate, totalFrames: totalFrames, totalElapsed: elapsed,
                halfFrames: firstHalfFrames, halfElapsed: firstHalfElapsed)
                ? rate : nil
        }

        guard let rate = rateOrNil else {
            // Either nowhere near a standard rate, or a window that straddled
            // a change. Distrust it rather than latch a number nothing can
            // clock at, and break any pending revision — inconsistent
            // evidence is not evidence.
            pendingRate = nil
            pendingWindows = 0
            return standing
        }

        guard let current = settled else {
            settled = rate
            return (rate, true)
        }
        guard rate != current else {
            pendingRate = nil
            pendingWindows = 0
            return (current, false)
        }

        // A dissenting window. Require consecutive agreement before acting.
        if pendingRate == rate {
            pendingWindows += 1
        } else {
            pendingRate = rate
            pendingWindows = 1
        }
        guard pendingWindows >= Self.revisionsRequired else { return (current, false) }
        pendingRate = nil
        pendingWindows = 0
        settled = rate
        return (rate, true)
    }
}

/// System-audio capture via the Core Audio process-tap API (macOS 14.4+).
///
/// Replaces the ScreenCaptureKit loopback path: a process tap records the
/// audio *output* of every process except Dimmy itself, which is exactly
/// the "system audio / other call participants" signal meeting mode wants.
/// Crucially it asks only for the audio-recording TCC grant (the discreet
/// purple dot) — NOT Screen Recording — so users stop seeing the spurious
/// "video / screen" permission prompt, and macOS Sequoia's media-library
/// prompt (triggered by SCStream when Music/TV produce audio) never fires.
///
/// Shape: build a global mono tap excluding our own pid → wrap it in a
/// private aggregate device → install an IO proc whose block forwards the
/// tapped f32 PCM to the Rust core via `dimmy_push_loopback_audio`, the
/// same FFI the SCStream path used. The core mixes it with the mic through
/// AEC3 at the 48 kHz canonical rate and writes `audio_system.wav` — none
/// of that changes. `muteBehavior = .unmuted` keeps the call audible while
/// we tap it.
///
/// Lifecycle is synchronous (all CoreAudio HAL calls); `SystemAudioCapture`
/// `Service` drives it on 14.4+ and falls back to SCStream when `start()`
/// returns false (tap creation denied / older OS / no default output).
@available(macOS 14.4, *)
final class SystemAudioProcessTap {
    /// Forwarded on the CoreAudio IO thread: (mono f32 samples, count, rate).
    /// Set before `start()`. Must be cheap + lock-free — it runs on a
    /// realtime audio callback.
    var onSamples: ((UnsafePointer<Float>, Int, Int32) -> Void)?

    private var tapID = AudioObjectID(kAudioObjectUnknown)
    private var aggregateID = AudioObjectID(kAudioObjectUnknown)
    private var ioProcID: AudioDeviceIOProcID?
    private var sampleRate: Int32 = 48_000
    private var channelCount: Int = 1
    private var running = false

    /// PID set the current tap was built from. Used by the re-enumerate
    /// tick to detect "the active-audio process set changed, rebuild the
    /// tap" without paying for a teardown when nothing changed. Also
    /// exposed (read-only) for diagnostics and for CallDetectionManager
    /// to read the currently-captured set as a presence signal.
    private(set) var currentTapPidSet: Set<pid_t> = []

    /// Default-output UID the aggregate device was anchored to at build
    /// time. The aggregate carries the tap and uses this device as its
    /// clock anchor + sub-device (set at lines ~159-162 below). When
    /// macOS flips the default output mid-meeting (BT headphones
    /// connect/disconnect, wired unplug/replug, Sound prefs change), the
    /// aggregate keeps pointing at the stale device and the tap silently
    /// delivers silence — same root cause as the Windows WASAPI loopback
    /// bug fixed in 80540e3. Compared against the live default in
    /// `rescanAndRebuildIfNeeded` to trigger a rebuild on change.
    /// nil while deferred or torn down.
    private(set) var builtOutputUID: String?

    /// Latched true the first time the IO proc fires. Diagnostic only —
    /// callers no longer key any recovery on this. The capture service
    /// trusts `.live` from `start()`; if the HAL ever stops delivering
    /// frames the listener-driven rebuild path (process death / default
    /// output change) takes over without timer-driven probes.
    private let receivedAudioFlag = OSAllocatedUnfairLock(initialState: false)
    var hasReceivedAudio: Bool { receivedAudioFlag.withLock { $0 } }

    /// Monotonic IO-proc fire counter, bumped on EVERY callback (even the
    /// zero-filled buffers delivered during app silence). Unlike
    /// `hasReceivedAudio` (latched once, diagnostic), this is a live
    /// heartbeat: while the aggregate's clock device runs, the IO proc fires
    /// every cycle and this advances; it FREEZES the instant the tap dies.
    /// The liveness watchdog polls it to tell "nobody is playing audio"
    /// (still advancing) apart from "the tap is dead" (frozen) — the latter is
    /// what a sleep/wake HAL reset causes. RT-safe: the audio thread only
    /// increments, under the same unfair-lock kind as `receivedAudioFlag`.
    private let frameCounter = OSAllocatedUnfairLock<UInt64>(initialState: 0)
    var frameCount: UInt64 { frameCounter.withLock { $0 } }

    /// Measures what the tap ACTUALLY delivers, so the rate handed to the
    /// core is observed rather than believed. Reset on every teardown so a
    /// rebuilt tap (default-output change, sleep/wake) re-measures against
    /// whatever device it just got anchored to. Same unfair-lock kind as the
    /// counters above: the audio thread only reads/updates a few scalars.
    private let rateEstimator = OSAllocatedUnfairLock(initialState: LoopbackRateEstimator())

    /// The measured rate once settled, else nil. Diagnostics only.
    var measuredSampleRate: Int32? { rateEstimator.withLock { $0.settled } }

    /// mach ticks per second for this machine. Computed once, off the audio
    /// thread — `mach_timebase_info` must not run in an IO proc.
    private static let hostTicksPerSecond: Double = {
        var timebase = mach_timebase_info_data_t()
        guard mach_timebase_info(&timebase) == KERN_SUCCESS,
            timebase.numer > 0, timebase.denom > 0
        else {
            // 1 tick == 1 ns. Wrong only if the timebase call fails, in which
            // case the reading lands nowhere near a standard rate and the
            // estimator declines to decide — we keep the declared rate.
            return 1_000_000_000
        }
        return 1_000_000_000.0 * Double(timebase.denom) / Double(timebase.numer)
    }()

    private let ioQueue = DispatchQueue(
        label: "dimmy.systemaudio.tap.io", qos: .userInteractive)

    /// Outcome of a `start()` attempt.
    ///
    /// - `.live`: the tap is recording — IO proc is firing, samples flow.
    /// - `.deferred`: no audio source was active at start; the rescan timer
    ///   will pick up the first source within ~3 s and self-promote to
    ///   `.live`. The caller should keep the instance alive — falling back
    ///   to SCKit here would defeat the per-process design.
    /// - `.failed`: HAL error — caller should fall back to SCStream.
    enum StartOutcome { case live, deferred, failed }

    /// Create the tap + aggregate device + IO proc and start pulling audio.
    /// Idempotent: a second call while running is a no-op (returns `.live`).
    ///
    /// `.deferred` is NOT a failure — the instance is kept hot and the
    /// rescan timer will promote it the moment an audio source appears.
    /// Only `.failed` should trigger SCStream fallback.
    @discardableResult
    func start() -> StartOutcome {
        guard !running else { return .live }

        // Tap a SINGLE process — the pattern Apple's AudioCap reference +
        // Notion + AudioTee all use. The historic "mono mixdown of N>1
        // processes" variant has shown intermittent zero-buffer behaviour
        // across recent macOS releases; one-process-at-a-time tap fires
        // reliably and avoids the entire class of issues.
        //
        // Selection: the FIRST audio-active process the HAL returns. No
        // priority list, no scoring, no known-app filter — the listener
        // path below (kAudioHardwarePropertyProcessObjectList +
        // per-process kAudioProcessPropertyIsRunning) rebuilds the tap
        // automatically when the active set changes, so re-selecting on
        // every change keeps us pointed at whatever app the user is
        // currently making sound with.
        let selfPid = ProcessInfo.processInfo.processIdentifier
        let activeObjects = Self.audioActiveProcessObjects(excludingSelf: selfPid)
        // Prefer a known call app (Teams / Zoom / Webex / Meet) among the
        // audio-active processes over "whatever the HAL returns first" — so a
        // meeting always taps the CALL, not a music player / notification that
        // happens to be first in the list. Still a SINGLE process: our N>1
        // mixdown showed intermittent zero buffers, so we keep one target.
        // When no known call app is producing audio (e.g. recording a YouTube
        // tab) we fall back to the first active process = today's behaviour,
        // so the generic case is unchanged. NOTE: this is NOT the reverted
        // gate-or-break heuristic — there is no broken global-mixdown fallback;
        // worst case we tap `.first` exactly like now.
        let callObj = activeObjects.first { obj in
            guard let p = Self.pid(forAudioObject: obj) else { return false }
            return CallDetectionManager.resolveKnownCallApp(p) != nil
        }
        guard let target = callObj ?? activeObjects.first else {
            // No app is currently producing audio. Don't create a dead tap;
            // keep the rescan listeners armed so we self-recover the moment
            // audio appears (.deferred → .live promotion in
            // rescanAndRebuildIfNeeded). The caller treats .deferred as
            // success and holds onto the instance.
            dimmyHostLog("[SystemAudio/tap] no audio-active processes at start() — deferred (rescan will promote)")
            startRescan()
            return .deferred
        }
        let targetPid = Self.pid(forAudioObject: target) ?? -1
        let targetKind = callObj != nil ? "call-app" : "first-active"
        dimmyHostLog("[SystemAudio/tap] tapping SINGLE \(targetKind) process pid=\(targetPid) (of \(activeObjects.count) active)")

        let description = CATapDescription(monoMixdownOfProcesses: [target])
        description.uuid = UUID()
        description.name = "Dimmy System Audio"
        description.muteBehavior = .unmuted
        description.isPrivate = true
        description.isExclusive = false

        var newTap = AudioObjectID(kAudioObjectUnknown)
        var err = AudioHardwareCreateProcessTap(description, &newTap)
        guard err == noErr, newTap != AudioObjectID(kAudioObjectUnknown) else {
            NSLog("[SystemAudio/tap] AudioHardwareCreateProcessTap failed: %d", err)
            return .failed
        }
        tapID = newTap
        currentTapPidSet = targetPid > 0 ? Set([targetPid]) : Set()

        guard let asbd = Self.readTapFormat(tapID) else {
            NSLog("[SystemAudio/tap] could not read tap stream format")
            teardown()
            return .failed
        }
        sampleRate = asbd.mSampleRate > 0 ? Int32(asbd.mSampleRate) : 48_000
        channelCount = asbd.mChannelsPerFrame > 0 ? Int(asbd.mChannelsPerFrame) : 1

        // Pre-publish the rate so the meeting worker's STT downsample uses
        // the right source rate before the first buffer lands (the rate on
        // each push wins if it ever differs).
        _ = dimmy_set_loopback_sample_rate(sampleRate)

        // The aggregate needs a clock to drive its IO proc. A tap-only
        // aggregate (empty sub-device list) is created fine but its IO proc
        // never fires — observed as `samples=0`. Anchor it to the default
        // output device (kept private, so the user's output route is
        // untouched) and drift-compensate the tap against that clock; this
        // is the configuration Apple's own tap sample uses.
        let outputUID = Self.defaultOutputDeviceUID()
        builtOutputUID = outputUID
        NSLog("[SystemAudio/tap] tap format rate=%d ch=%d; clock anchor outputUID=%@",
              sampleRate, channelCount, outputUID ?? "<none>")
        let aggregateUID = UUID().uuidString
        var aggregateDescription: [String: Any] = [
            kAudioAggregateDeviceNameKey: "Dimmy System Audio Tap",
            kAudioAggregateDeviceUIDKey: aggregateUID,
            kAudioAggregateDeviceIsPrivateKey: true,
            kAudioAggregateDeviceIsStackedKey: false,
            // Tahoe (macOS 26.x) HAL regression: when `TapAutoStart=true`, the
            // aggregate registers but its IO proc never fires (observed on 26.2:
            // `HALS_MultiTap::register_autostart_context` succeeds, then a flood
            // of `HandleRecordingStatusChangeForIsolatedIO: No kDeviceInput
            // streams` and zero samples ever reach the callback). With
            // `TapAutoStart=false` + the explicit `AudioDeviceStart` below, the
            // IO proc fires reliably across 14.4–26.2. Verified 2026-05-29 via
            // `runMultiConfigProbe` (V0=samples=0 vs V1=samples=142848 in 3 s).
            // Chromium upstream uses `false` for the same reason.
            kAudioAggregateDeviceTapAutoStartKey: false,
            kAudioAggregateDeviceTapListKey: [[
                kAudioSubTapDriftCompensationKey: true,
                kAudioSubTapUIDKey: description.uuid.uuidString,
            ]],
        ]
        if let outputUID {
            aggregateDescription[kAudioAggregateDeviceMainSubDeviceKey] = outputUID
            aggregateDescription[kAudioAggregateDeviceSubDeviceListKey] =
                [[kAudioSubDeviceUIDKey: outputUID]]
        } else {
            aggregateDescription[kAudioAggregateDeviceSubDeviceListKey] = [[String: Any]]()
        }

        var newAggregate = AudioObjectID(kAudioObjectUnknown)
        err = AudioHardwareCreateAggregateDevice(
            aggregateDescription as CFDictionary, &newAggregate)
        guard err == noErr, newAggregate != AudioObjectID(kAudioObjectUnknown) else {
            NSLog("[SystemAudio/tap] AudioHardwareCreateAggregateDevice failed: %d", err)
            teardown()
            return .failed
        }
        aggregateID = newAggregate

        // Pin the aggregate to the canonical 48 kHz. THE fix for the
        // "3x-fast / chopped system audio" bug: a Bluetooth-HFP output only
        // clocks at 16 kHz, so a tap anchored to it delivered 16 kHz of
        // content while `readTapFormat` still reported the nominal 48 kHz —
        // the core trusted the claim, did passthrough, and the meeting
        // worker zero-filled the 2/3 shortfall (the gated "a tratti" the
        // recovery uncovered on 2026-07-21). Setting the aggregate's nominal
        // rate makes CoreAudio sample-rate-convert the HFP sub-device UP to
        // 48 kHz internally, so the IO proc ACTUALLY delivers 48 kHz and the
        // core never has to guess a rate. This is what Windows gets for free
        // via WASAPI shared-mode. If the set fails we keep the tap-format
        // rate (previous behaviour) rather than abort the capture.
        var nominalRateAddr = AudioObjectPropertyAddress(
            mSelector: kAudioDevicePropertyNominalSampleRate,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain)
        var wantRate: Float64 = 48_000
        let rateErr = AudioObjectSetPropertyData(
            aggregateID, &nominalRateAddr, 0, nil,
            UInt32(MemoryLayout<Float64>.size), &wantRate)
        if rateErr == noErr {
            sampleRate = 48_000
            NSLog("[SystemAudio/tap] aggregate pinned to 48000 Hz (CoreAudio SRC handles the sub-device rate)")
        } else {
            NSLog("[SystemAudio/tap] could not pin aggregate to 48000 Hz (err %d) — using tap-format rate %d",
                  rateErr, sampleRate)
        }
        // Re-publish the (now canonical) rate so the meeting worker sizes the
        // system WAV header + STT downsample against what we actually deliver.
        _ = dimmy_set_loopback_sample_rate(sampleRate)

        // Capture into the realtime block by value — no self capture, no
        // allocation on the mono fast path.
        let handler = onSamples
        let rate = sampleRate
        let channels = channelCount
        let receivedFlag = receivedAudioFlag
        let frames = frameCounter
        let estimator = rateEstimator
        let ticksPerSecond = Self.hostTicksPerSecond
        var newProcID: AudioDeviceIOProcID?
        err = AudioDeviceCreateIOProcIDWithBlock(&newProcID, aggregateID, ioQueue) {
            _, inInputData, inInputTime, _, _ in
            // Heartbeat: bump on EVERY fire (even silent/zero buffers) so the
            // liveness watchdog can see the IO proc is alive and distinguish a
            // quiet-but-healthy tap from a dead one.
            frames.withLock { $0 &+= 1 }
            // Latch + detect the first fire so we can log "capture is live"
            // exactly once. Diagnostic only — no recovery logic keys on it.
            let firstFire = receivedFlag.withLock { (state: inout Bool) -> Bool in
                let wasSet = state
                state = true
                return !wasSet
            }
            if firstFire {
                NSLog("[SystemAudio/tap] IO proc fired (first frame, %d samples) — capture is live",
                      Self.firstBufferSampleCount(inInputData))
            }
            // The rate the aggregate DECLARED is a starting assumption; what
            // it actually delivers is measured from the timestamps below.
            let effectiveRate = Self.effectiveRate(
                declared: rate,
                inInputTime: inInputTime,
                inInputData: inInputData,
                ticksPerSecond: ticksPerSecond,
                estimator: estimator)
            Self.forward(inInputData, channels: channels, rate: effectiveRate, to: handler)
        }
        guard err == noErr, let procID = newProcID else {
            NSLog("[SystemAudio/tap] AudioDeviceCreateIOProcIDWithBlock failed: %d", err)
            teardown()
            return .failed
        }
        ioProcID = procID

        err = AudioDeviceStart(aggregateID, procID)
        guard err == noErr else {
            NSLog("[SystemAudio/tap] AudioDeviceStart failed: %d", err)
            teardown()
            return .failed
        }

        running = true
        NSLog("[SystemAudio/tap] started rate=%d ch=%d", rate, channels)
        startRescan()
        return .live
    }

    func stop() {
        stopRescan()
        guard running else { return }
        teardown()
        running = false
        NSLog("[SystemAudio/tap] stopped")
    }

    // MARK: - Rescan (event-driven HAL listeners + safety backstop)

    /// Watch the audio-active process set and rebuild the tap when it
    /// changes. Event-driven primary path: two `AudioObjectAddProperty-`
    /// `ListenerBlock` subscriptions notify us in real time
    /// (latency ~5-20 ms instead of the historical 3 s polling tick):
    ///
    ///   1. `kAudioObjectSystemObject` /
    ///      `kAudioHardwarePropertyProcessObjectList` — fires whenever a
    ///      process registers or de-registers with coreaudiod (any app
    ///      opens its first audio stream or closes its last one).
    ///   2. Each `AudioProcess` object /
    ///      `kAudioProcessPropertyIsRunning` — fires when that process
    ///      starts or stops producing audio output.
    ///
    /// Both callbacks converge on `rescanAndRebuildIfNeeded()`, whose
    /// `Set == currentTapPidSet` guard makes double-fire harmless. The
    /// per-process subscriptions are refreshed every time the system
    /// listener fires (new objects subscribed, vanished objects pruned).
    ///
    /// 30 s `DispatchSourceTimer` backstop runs in parallel — it's
    /// idempotent with the listeners (same Set-equality guard) so a
    /// missed event (HAL bugs, transient registration failure) is
    /// silently recovered within 30 s instead of stranding us. Cost is
    /// one HAL list read + per-process flag reads, ~2 ms every 30 s.
    ///
    /// Both the listener queue and the backstop queue are `ioQueue` —
    /// the same serial queue the IO proc handler fires on — so all
    /// rescan-related work is naturally serialized against IO proc
    /// events. Listener-state mutations go through `listenerLock` so
    /// `start()` / `stop()` from main and the listener block on ioQueue
    /// don't race on the dictionary.
    private var processListListener: AudioObjectPropertyListenerBlock?
    private var perProcessListeners: [AudioObjectID: AudioObjectPropertyListenerBlock] = [:]
    /// Listener on `kAudioHardwarePropertyDefaultOutputDevice`. Fires when
    /// macOS flips the default output (BT (dis)connect, wired unplug/replug,
    /// Sound prefs change). The handler funnels to `rescanAndRebuildIfNeeded`
    /// which rebuilds the tap+aggregate against the new default — same
    /// machinery as the PID-set rebuild. See `builtOutputUID` doc above.
    private var defaultOutputListener: AudioObjectPropertyListenerBlock?
    private var rescanBackstop: DispatchSourceTimer?
    private let listenerLock = NSLock()

    /// Liveness watchdog — rebuilds the tap when its IO proc STOPS firing
    /// while we still expect audio (a target PID is captured). This is the
    /// self-heal for the sleep/wake dead-tap bug: after wake, CoreAudio
    /// re-publishes the default-output device the aggregate is clock-anchored
    /// to WITHOUT changing its UID or the audio-active PID set, so the
    /// change-based rebuild guard in `rescanAndRebuildIfNeeded` never fires
    /// and the tap sits silent until an app restart. The watchdog keys off the
    /// IO-proc heartbeat (`frameCounter`), not device/PID changes, so it
    /// catches a silently-dead tap regardless of why it died.
    private var watchdogTimer: DispatchSourceTimer?
    private var watchdogLastCount: UInt64 = 0
    private var watchdogLastAdvance = Date()
    private var watchdogRebuilds = 0
    /// Seconds of frozen heartbeat before the tap is declared dead. Long
    /// enough to clear the brief start-up window before the first frame, short
    /// enough to recover within a few seconds of a wake.
    private static let tapStaleThreshold: TimeInterval = 4.0
    /// Stop force-rebuilding after this many consecutive dead cycles: a tap
    /// that won't recover after ~N*threshold s is a genuine failure (revoked
    /// permission, no output device) a rebuild can't fix, and thrashing
    /// coreaudiod only makes it worse. A single heartbeat advance resets the
    /// counter, so a later recovery re-arms the watchdog.
    private static let maxWatchdogRebuilds = 8

    /// Re-arm the tap when the Mac wakes from sleep — immediate recovery on
    /// top of the watchdog backstop. macOS tears down and re-publishes HAL
    /// device objects across sleep, leaving the tap's aggregate clock-anchored
    /// to a stale endpoint that delivers silence. Mirrors the CGEvent-tap wake
    /// re-arm in HotkeyManager.
    private var wakeObserver: NSObjectProtocol?

    private func startRescan() {
        // Idempotent: rescan handler runs `_ = start()` to rebuild the
        // tap, which re-enters `startRescan()`. Listeners survive tap
        // rebuilds (they're on the system object + process objects,
        // not on the tap), so the re-entry must be a no-op.
        listenerLock.lock()
        let alreadyArmed = processListListener != nil
        listenerLock.unlock()
        if alreadyArmed { return }

        var sysAddr = AudioObjectPropertyAddress(
            mSelector: kAudioHardwarePropertyProcessObjectList,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain)
        let sysBlock: AudioObjectPropertyListenerBlock = { [weak self] _, _ in
            self?.handleProcessListChanged()
        }
        let st = AudioObjectAddPropertyListenerBlock(
            AudioObjectID(kAudioObjectSystemObject), &sysAddr, ioQueue, sysBlock)
        if st == noErr {
            listenerLock.lock()
            processListListener = sysBlock
            listenerLock.unlock()
            NSLog("[SystemAudio/tap] event listener armed on ProcessObjectList")
        } else {
            NSLog("[SystemAudio/tap] AddPropertyListener(ProcessObjectList) failed: %d — backstop polling only", st)
        }

        refreshPerProcessListeners()

        // Default-output listener: rebuild when macOS flips the default output
        // mid-tap, so the aggregate's clock anchor follows the new device
        // instead of capturing silence from a stale endpoint. Win parity with
        // 80540e3 (loopback follows default output on device change).
        var defaultOutAddr = AudioObjectPropertyAddress(
            mSelector: kAudioHardwarePropertyDefaultOutputDevice,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain)
        let defaultOutBlock: AudioObjectPropertyListenerBlock = { [weak self] _, _ in
            self?.rescanAndRebuildIfNeeded()
        }
        let stDef = AudioObjectAddPropertyListenerBlock(
            AudioObjectID(kAudioObjectSystemObject), &defaultOutAddr, ioQueue, defaultOutBlock)
        if stDef == noErr {
            listenerLock.lock()
            defaultOutputListener = defaultOutBlock
            listenerLock.unlock()
            NSLog("[SystemAudio/tap] event listener armed on DefaultOutputDevice")
        } else {
            NSLog("[SystemAudio/tap] AddPropertyListener(DefaultOutputDevice) failed: %d — backstop polling only", stDef)
        }

        let backstop = DispatchSource.makeTimerSource(queue: ioQueue)
        backstop.schedule(deadline: .now() + 30.0, repeating: 30.0)
        backstop.setEventHandler { [weak self] in self?.rescanAndRebuildIfNeeded() }
        backstop.resume()
        rescanBackstop = backstop

        // Liveness watchdog (see property doc). Polls the IO-proc heartbeat
        // and rebuilds a silently-dead tap that the change-based path misses —
        // notably after sleep/wake, where the default-output UID + PID set are
        // unchanged so no listener fires. Runs on ioQueue so it serializes with
        // the IO proc + the listener-driven rebuilds. Armed once (this whole
        // block is past the `alreadyArmed` guard), survives tap rebuilds.
        watchdogLastCount = frameCount
        watchdogLastAdvance = Date()
        watchdogRebuilds = 0
        let watchdog = DispatchSource.makeTimerSource(queue: ioQueue)
        watchdog.schedule(
            deadline: .now() + Self.tapStaleThreshold,
            repeating: 2.0, leeway: .milliseconds(500))
        watchdog.setEventHandler { [weak self] in self?.checkTapLiveness() }
        watchdog.resume()
        watchdogTimer = watchdog

        // Immediate wake re-arm, mirroring HotkeyManager's didWakeNotification
        // handler. The block hops to ioQueue so the rebuild serializes with
        // everything else; the watchdog is the backstop if this fires before
        // the HAL has settled and the freshly-rebuilt tap is itself born dead.
        wakeObserver = NSWorkspace.shared.notificationCenter.addObserver(
            forName: NSWorkspace.didWakeNotification, object: nil, queue: .main
        ) { [weak self] _ in
            guard let self else { return }
            self.ioQueue.async {
                guard self.running, !self.currentTapPidSet.isEmpty else { return }
                NSLog("[SystemAudio/tap] system woke — rebuilding system-audio tap")
                self.watchdogLastAdvance = Date()
                self.watchdogLastCount = 0
                self.watchdogRebuilds = 0
                self.forceRebuild()
            }
        }
    }

    private func stopRescan() {
        listenerLock.lock()
        let oldSys = processListListener
        let oldPerProcess = perProcessListeners
        let oldDefaultOut = defaultOutputListener
        processListListener = nil
        perProcessListeners.removeAll()
        defaultOutputListener = nil
        listenerLock.unlock()

        if let block = oldSys {
            var sysAddr = AudioObjectPropertyAddress(
                mSelector: kAudioHardwarePropertyProcessObjectList,
                mScope: kAudioObjectPropertyScopeGlobal,
                mElement: kAudioObjectPropertyElementMain)
            _ = AudioObjectRemovePropertyListenerBlock(
                AudioObjectID(kAudioObjectSystemObject), &sysAddr, ioQueue, block)
        }
        for (obj, block) in oldPerProcess {
            var addr = AudioObjectPropertyAddress(
                mSelector: kAudioProcessPropertyIsRunning,
                mScope: kAudioObjectPropertyScopeGlobal,
                mElement: kAudioObjectPropertyElementMain)
            _ = AudioObjectRemovePropertyListenerBlock(obj, &addr, ioQueue, block)
        }
        if let block = oldDefaultOut {
            var defaultOutAddr = AudioObjectPropertyAddress(
                mSelector: kAudioHardwarePropertyDefaultOutputDevice,
                mScope: kAudioObjectPropertyScopeGlobal,
                mElement: kAudioObjectPropertyElementMain)
            _ = AudioObjectRemovePropertyListenerBlock(
                AudioObjectID(kAudioObjectSystemObject), &defaultOutAddr, ioQueue, block)
        }
        watchdogTimer?.cancel()
        watchdogTimer = nil
        if let wakeObserver {
            NSWorkspace.shared.notificationCenter.removeObserver(wakeObserver)
            self.wakeObserver = nil
        }
        rescanBackstop?.cancel()
        rescanBackstop = nil
    }

    /// System listener handler — runs on ioQueue. Refresh per-process
    /// subscriptions (new processes appeared or old ones vanished),
    /// then re-check the rescan condition. Both steps are cheap and
    /// idempotent.
    private func handleProcessListChanged() {
        refreshPerProcessListeners()
        rescanAndRebuildIfNeeded()
    }

    /// Subscribe to `kAudioProcessPropertyIsRunning` for every audio
    /// process the HAL knows about (minus our own), and unsubscribe
    /// from objects that have vanished. Called from `startRescan` on
    /// initial setup and from the system listener every time the
    /// process list changes.
    private func refreshPerProcessListeners() {
        let allObjects = Set(Self.allAudioProcessObjects())
        let selfPid = ProcessInfo.processInfo.processIdentifier

        listenerLock.lock()
        // Drop listeners for processes that have vanished.
        let gone = perProcessListeners.keys.filter { !allObjects.contains($0) }
        for obj in gone {
            if let block = perProcessListeners[obj] {
                var addr = AudioObjectPropertyAddress(
                    mSelector: kAudioProcessPropertyIsRunning,
                    mScope: kAudioObjectPropertyScopeGlobal,
                    mElement: kAudioObjectPropertyElementMain)
                _ = AudioObjectRemovePropertyListenerBlock(obj, &addr, ioQueue, block)
            }
            perProcessListeners.removeValue(forKey: obj)
        }

        // Subscribe to new ones (skip self — Dimmy's own output
        // is irrelevant for the tap and would self-trigger rebuilds).
        for obj in allObjects where perProcessListeners[obj] == nil {
            if Self.pid(forAudioObject: obj) == selfPid { continue }
            var addr = AudioObjectPropertyAddress(
                mSelector: kAudioProcessPropertyIsRunning,
                mScope: kAudioObjectPropertyScopeGlobal,
                mElement: kAudioObjectPropertyElementMain)
            let block: AudioObjectPropertyListenerBlock = { [weak self] _, _ in
                self?.rescanAndRebuildIfNeeded()
            }
            let st = AudioObjectAddPropertyListenerBlock(obj, &addr, ioQueue, block)
            if st == noErr {
                perProcessListeners[obj] = block
            }
        }
        listenerLock.unlock()
    }

    /// Pure decision: does a default-output change warrant a tap rebuild?
    /// Win parity with `loopback_should_follow_default` (audio.rs). Same
    /// intent: only rebuild when we have a known current default that
    /// differs from the one the aggregate was anchored to. If the system
    /// currently has no default output (rare transient state mid-unplug),
    /// suppress to avoid thrashing — the next listener fire will catch
    /// the new default and trigger the rebuild then.
    static func shouldRebuildForOutputChange(
        builtUID: String?, currentUID: String?
    ) -> Bool {
        guard let current = currentUID else { return false }
        return current != builtUID
    }

    private func rescanAndRebuildIfNeeded() {
        let selfPid = ProcessInfo.processInfo.processIdentifier
        let activeObjects = Self.audioActiveProcessObjects(excludingSelf: selfPid)
        let newPidSet = Set(activeObjects.compactMap { Self.pid(forAudioObject: $0) })

        if running {
            // Live tap: rebuild when EITHER the active PID set or the default
            // output device has changed. Same-state tick costs one HAL list
            // read + per-process flag + Set equality + UID string compare —
            // cheap enough to do at 30 s backstop cadence forever.
            let currentOutputUID = Self.defaultOutputDeviceUID()
            let outputChanged = Self.shouldRebuildForOutputChange(
                builtUID: builtOutputUID, currentUID: currentOutputUID)
            let pidSetChanged = newPidSet != currentTapPidSet
            guard pidSetChanged || outputChanged else { return }
            if outputChanged {
                NSLog("[SystemAudio/tap] default output changed (%@ -> %@) — rebuilding tap",
                      builtOutputUID ?? "<none>", currentOutputUID ?? "<none>")
            }
            if pidSetChanged {
                NSLog("[SystemAudio/tap] audio-active PID set changed (was %d, now %d) — rebuilding tap",
                      currentTapPidSet.count, newPidSet.count)
            }
            forceRebuild()
        } else {
            // Deferred state: no tap created yet (no audio source at start).
            // Wait for ANY audio source, then promote to live. Avoids the
            // false-positive SCKit fallback when the user starts a meeting
            // before opening their videoconf app. Default-output changes
            // while deferred are no-ops — `start()` reads the live default
            // when it eventually fires.
            guard !activeObjects.isEmpty else { return }
            NSLog("[SystemAudio/tap] audio now active (%d source(s)) — promoting deferred tap to live",
                  activeObjects.count)
            _ = start()
        }
    }

    /// Full teardown + rebuild of the tap, BYPASSING the "did the PID set or
    /// default output change" guard in `rescanAndRebuildIfNeeded`. Used by the
    /// change-based rebuild, the liveness watchdog, and the wake observer —
    /// the latter two fire when the tap went silent with NO observable
    /// device/PID change (a sleep/wake HAL reset re-publishes the same
    /// output-device UID). Runs on ioQueue, like every other rebuild path.
    private func forceRebuild() {
        guard running else { return }
        let savedHandler = onSamples
        teardown()
        running = false
        onSamples = savedHandler
        _ = start()
    }

    /// Liveness watchdog tick (ioQueue). Rebuilds the tap if its IO proc has
    /// stopped firing while a capture target is still expected. A healthy tap
    /// fires every IO cycle (even during silence), so its heartbeat keeps
    /// advancing and this never triggers; only a dead tap (frozen heartbeat)
    /// does. Across sleep the wall-clock `watchdogLastAdvance` is hours stale,
    /// so the first post-wake tick rebuilds immediately.
    private func checkTapLiveness() {
        // Only watch when live AND holding a target. A deferred tap (no audio
        // source yet) legitimately produces no frames; the rescan path
        // promotes it when a source appears — don't fight that here.
        guard running, !currentTapPidSet.isEmpty else {
            watchdogLastCount = frameCount
            watchdogLastAdvance = Date()
            watchdogRebuilds = 0
            return
        }
        let count = frameCount
        if count != watchdogLastCount {
            // IO proc is firing — tap alive (even if the buffers are silent).
            watchdogLastCount = count
            watchdogLastAdvance = Date()
            watchdogRebuilds = 0
            return
        }
        // Heartbeat frozen — for how long?
        let stalled = Date().timeIntervalSince(watchdogLastAdvance)
        guard stalled >= Self.tapStaleThreshold else { return }
        if watchdogRebuilds >= Self.maxWatchdogRebuilds {
            // Give up: a rebuild won't fix a genuine failure and thrashing
            // coreaudiod makes it worse. Log once (guarded by ==), then stay
            // quiet until a heartbeat advance resets watchdogRebuilds.
            if watchdogRebuilds == Self.maxWatchdogRebuilds {
                NSLog("[SystemAudio/tap] watchdog: tap still dead after %d rebuilds — giving up until it recovers (restart Dimmy if system audio stays missing)",
                      watchdogRebuilds)
                watchdogRebuilds += 1
            }
            return
        }
        watchdogRebuilds += 1
        NSLog("[SystemAudio/tap] watchdog: no IO-proc frames for %.1fs while capturing %d process(es) — tap is dead, rebuilding (attempt %d/%d)",
              stalled, currentTapPidSet.count, watchdogRebuilds, Self.maxWatchdogRebuilds)
        forceRebuild()
        // Give the freshly-rebuilt tap a full grace window before re-judging.
        watchdogLastAdvance = Date()
        watchdogLastCount = frameCount
    }

    /// Sample count in the first buffer of an IO proc's input list.
    /// Used only by the one-time "capture is live" diagnostic log.
    private static func firstBufferSampleCount(
        _ inInputData: UnsafePointer<AudioBufferList>
    ) -> Int {
        let abl = UnsafeMutableAudioBufferListPointer(
            UnsafeMutablePointer(mutating: inInputData))
        guard let buffer = abl.first else { return 0 }
        return Int(buffer.mDataByteSize) / MemoryLayout<Float>.size
    }

    /// Frames (per-channel sample count) actually delivered in this callback.
    /// Mirrors `forward`'s own channel handling so the measurement counts the
    /// same thing the core will receive.
    private static func deliveredFrameCount(
        _ inInputData: UnsafePointer<AudioBufferList>
    ) -> Int {
        let abl = UnsafeMutableAudioBufferListPointer(
            UnsafeMutablePointer(mutating: inInputData))
        guard let buffer = abl.first else { return 0 }
        let channels = max(Int(buffer.mNumberChannels), 1)
        return (Int(buffer.mDataByteSize) / MemoryLayout<Float>.size) / channels
    }

    /// The rate to hand the core for this callback: the MEASURED delivery
    /// rate once it has settled, the DECLARED one until then — and forever if
    /// the HAL gives us no usable host time. Never worse than the old
    /// behaviour, which was to always report the declaration.
    ///
    /// Runs on the CoreAudio IO thread. The `NSLog` fires only on a verdict
    /// TRANSITION — once when the rate is first measured, and once more if a
    /// Bluetooth profile flip moves it mid-meeting. That line is the
    /// fingerprint of a lying tap and the first thing to look for when a
    /// meeting comes back sounding wrong.
    private static func effectiveRate(
        declared: Int32,
        inInputTime: UnsafePointer<AudioTimeStamp>,
        inInputData: UnsafePointer<AudioBufferList>,
        ticksPerSecond: Double,
        estimator: OSAllocatedUnfairLock<LoopbackRateEstimator>
    ) -> Int32 {
        let timestamp = inInputTime.pointee
        guard timestamp.mFlags.contains(.hostTimeValid) else {
            return estimator.withLock { $0.settled } ?? declared
        }
        let verdict = estimator.withLock {
            $0.observe(
                deliveredFrames: deliveredFrameCount(inInputData),
                hostTime: timestamp.mHostTime,
                ticksPerSecond: ticksPerSecond)
        }
        guard let verdict else { return declared }
        if verdict.isNew {
            if verdict.rate == declared {
                NSLog(
                    "[SystemAudio/tap] measured delivery rate %d Hz — matches the declared rate",
                    verdict.rate)
            } else {
                NSLog(
                    "[SystemAudio/tap] WARN aggregate DECLARES %d Hz but DELIVERS %d Hz — using the measured rate (48 kHz pin ineffective, or a BT profile flip)",
                    declared, verdict.rate)
            }
        }
        return verdict.rate
    }

    // MARK: - Realtime forwarding

    /// Read the tapped buffer (float32) and forward a mono frame. Runs on
    /// the CoreAudio IO thread. The mono mixdown tap normally gives a single
    /// 1-channel buffer (zero-allocation path); the multi-channel branch is
    /// a defensive average in case the HAL hands us interleaved stereo.
    private static func forward(
        _ inInputData: UnsafePointer<AudioBufferList>,
        channels: Int,
        rate: Int32,
        to handler: ((UnsafePointer<Float>, Int, Int32) -> Void)?
    ) {
        guard let handler else { return }
        let abl = UnsafeMutableAudioBufferListPointer(
            UnsafeMutablePointer(mutating: inInputData))
        guard let buffer = abl.first, let data = buffer.mData else { return }
        let ch = max(Int(buffer.mNumberChannels), 1)
        let totalSamples = Int(buffer.mDataByteSize) / MemoryLayout<Float>.size
        guard totalSamples > 0 else { return }
        let floats = data.assumingMemoryBound(to: Float.self)

        if ch <= 1 {
            handler(floats, totalSamples, rate)
            return
        }
        let frames = totalSamples / ch
        guard frames > 0 else { return }
        var mono = [Float](repeating: 0, count: frames)
        for f in 0..<frames {
            var acc: Float = 0
            for c in 0..<ch { acc += floats[f * ch + c] }
            mono[f] = acc / Float(ch)
        }
        mono.withUnsafeBufferPointer { handler($0.baseAddress!, frames, rate) }
    }

    // MARK: - Teardown

    /// Dedicated serial queue for the blocking CoreAudio destroy calls in
    /// `teardown()`. See the teardown comment: these HAL ops can wedge
    /// forever on macOS 26 (Tahoe) and MUST NOT run on the caller's thread.
    private static let teardownQueue = DispatchQueue(label: "com.dimmy.tap.teardown")

    private func teardown() {
        currentTapPidSet = []
        builtOutputUID = nil
        // Reset the diagnostic "first fire" latch so a rebuilt instance
        // logs its first frame again.
        receivedAudioFlag.withLock { $0 = false }
        // Drop the measured rate: the rebuilt aggregate may be anchored to a
        // different device (that IS why most rebuilds happen), so the next
        // instance must measure again rather than inherit the old verdict.
        rateEstimator.withLock { $0 = LoopbackRateEstimator() }

        // Capture the HAL handles and null the instance fields SYNCHRONOUSLY
        // so the object reads as fully torn-down the instant this returns,
        // then perform the actual destroy OFF the caller's thread.
        //
        // The four AudioHardwareDestroy* / AudioDevice* calls serialize on
        // CoreAudio's process-global HAL lock, which WEDGES (never returns)
        // on macOS 26 (Tahoe) when a process tap is destroyed while the
        // default input device is mid-transition. `stop()` is invoked from
        // @MainActor SystemAudioCaptureService.stop() on every meeting stop
        // (pill / hotkey / menu / window), so a wedge here froze the ENTIRE
        // app — menu bar, windows, the red rec indicator that can never
        // clear — until force-quit (Francesco, macOS 26, 2026-07-06).
        // Running the destroys on a background serial queue degrades a HAL
        // wedge to a leaked aggregate/tap object (+ a stuck bg thread) at
        // worst; the caller — and the UI — never block.
        let procID = ioProcID
        let aggID = aggregateID
        let tID = tapID
        ioProcID = nil
        aggregateID = AudioObjectID(kAudioObjectUnknown)
        tapID = AudioObjectID(kAudioObjectUnknown)
        // Verification markers (both land in dimmy.log): if the log shows
        // "dispatched" then the app keeps running (and "[Meeting] stopped"
        // appears), the Tahoe-freeze fix is working — even if "completed"
        // never follows (that means the HAL wedged but we no longer block).
        NSLog("[SystemAudio/tap] teardown: HAL destroy dispatched off-thread (caller unblocked)")
        SystemAudioProcessTap.teardownQueue.async {
            SystemAudioProcessTap.destroyHALHandles(procID: procID, aggID: aggID, tID: tID)
            NSLog("[SystemAudio/tap] teardown: HAL destroy completed off-thread")
        }
    }

    /// Destroy the CoreAudio HAL handles. MUST run OFF the main thread:
    /// these four calls serialize on the process-global HAL lock, which
    /// wedges forever on macOS 26 (Tahoe) during a tap teardown that
    /// races a device transition — a main-thread caller freezes the whole
    /// app. The `assert` is the negative-space tripwire against ever
    /// re-introducing a main-thread HAL teardown (exactly what commit
    /// ab87dc1 did on 2026-07-03): it fires in Debug builds and the
    /// preflight-mac launch the instant this runs on the main thread.
    private static func destroyHALHandles(
        procID: AudioDeviceIOProcID?,
        aggID: AudioObjectID,
        tID: AudioObjectID
    ) {
        assert(
            !Thread.isMainThread,
            "CoreAudio HAL destroy must never run on the main thread — it wedges on macOS 26 (Tahoe)"
        )
        if let procID, aggID != AudioObjectID(kAudioObjectUnknown) {
            AudioDeviceStop(aggID, procID)
            AudioDeviceDestroyIOProcID(aggID, procID)
        }
        if aggID != AudioObjectID(kAudioObjectUnknown) {
            AudioHardwareDestroyAggregateDevice(aggID)
        }
        if tID != AudioObjectID(kAudioObjectUnknown) {
            AudioHardwareDestroyProcessTap(tID)
        }
    }

    // MARK: - HAL helpers

    /// Translate a pid to its CoreAudio process object id (needed for the
    /// tap's exclude list). nil if the pid has no audio process object.
    fileprivate static func processObject(for pid: pid_t) -> AudioObjectID? {
        var address = AudioObjectPropertyAddress(
            mSelector: kAudioHardwarePropertyTranslatePIDToProcessObject,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain)
        var pidValue = pid
        var object = AudioObjectID(kAudioObjectUnknown)
        var size = UInt32(MemoryLayout<AudioObjectID>.size)
        let err = AudioObjectGetPropertyData(
            AudioObjectID(kAudioObjectSystemObject), &address,
            UInt32(MemoryLayout<pid_t>.size), &pidValue, &size, &object)
        return (err == noErr && object != AudioObjectID(kAudioObjectUnknown)) ? object : nil
    }

    /// Enumerate every audio process object the HAL knows about. Returns
    /// AudioObjectIDs (not PIDs) usable directly in
    /// `CATapDescription(monoMixdownOfProcesses:)`.
    ///
    /// Each entry is a process that has at least one IO context registered
    /// with coreaudiod (currently or in the recent past). To narrow further
    /// to "actively producing output", check `kAudioProcessPropertyIsRunning`
    /// per object — see `audioActiveProcessObjects(excludingSelf:)`. To
    /// narrow to "currently capturing input", check the input-side property
    /// from CallDetectionManager — same HAL list, different per-object filter.
    static func allAudioProcessObjects() -> [AudioObjectID] {
        var address = AudioObjectPropertyAddress(
            mSelector: kAudioHardwarePropertyProcessObjectList,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain)
        var size: UInt32 = 0
        var err = AudioObjectGetPropertyDataSize(
            AudioObjectID(kAudioObjectSystemObject), &address, 0, nil, &size)
        guard err == noErr, size > 0 else { return [] }
        let count = Int(size) / MemoryLayout<AudioObjectID>.size
        var ids = [AudioObjectID](repeating: 0, count: count)
        err = ids.withUnsafeMutableBufferPointer { buf -> OSStatus in
            AudioObjectGetPropertyData(
                AudioObjectID(kAudioObjectSystemObject), &address,
                0, nil, &size, buf.baseAddress!)
        }
        guard err == noErr else { return [] }
        return ids
    }

    /// Read the `pid_t` for an audio process object.
    static func pid(forAudioObject obj: AudioObjectID) -> pid_t? {
        var address = AudioObjectPropertyAddress(
            mSelector: kAudioProcessPropertyPID,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain)
        var pid: pid_t = -1
        var size = UInt32(MemoryLayout<pid_t>.size)
        let err = AudioObjectGetPropertyData(obj, &address, 0, nil, &size, &pid)
        return (err == noErr && pid > 0) ? pid : nil
    }

    /// True when the process is currently producing audio (has an active
    /// IO context on the OUTPUT side). False otherwise — including processes
    /// that have an audio object but aren't actively playing. Used to prune
    /// the tap list to the apps we actually want to capture.
    static func isOutputRunning(_ obj: AudioObjectID) -> Bool {
        var address = AudioObjectPropertyAddress(
            mSelector: kAudioProcessPropertyIsRunning,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain)
        var value: UInt32 = 0
        var size = UInt32(MemoryLayout<UInt32>.size)
        let err = AudioObjectGetPropertyData(obj, &address, 0, nil, &size, &value)
        return err == noErr && value != 0
    }

    /// Build the tap input list: every audio-active process EXCEPT Dimmy
    /// itself. Returned as AudioObjectIDs ready for
    /// `CATapDescription(monoMixdownOfProcesses:)`.
    ///
    /// Empty result means no app is currently producing audio. The caller
    /// should treat this as "no-op for now" (don't create a tap) and re-poll
    /// later — the periodic re-enumerate tick in `start()` will pick up the
    /// first audio-producing app within ~3 s.
    static func audioActiveProcessObjects(excludingSelf selfPid: pid_t) -> [AudioObjectID] {
        let all = allAudioProcessObjects()
        return all.filter { obj in
            guard let p = pid(forAudioObject: obj), p != selfPid else { return false }
            return isOutputRunning(obj)
        }
    }

    /// PIDs of processes currently producing audio OUTPUT (not capturing
    /// input). Mirror of `CallDetectionManager.inputRunningPids()` but on
    /// the render side, sharing the same HAL enumeration. Powers two
    /// features: per-process tap input selection (this file) and
    /// known-call-app output-side presence detection (CallDetectionManager).
    static func outputRunningPids() -> Set<pid_t> {
        var result = Set<pid_t>()
        for obj in allAudioProcessObjects() where isOutputRunning(obj) {
            if let p = pid(forAudioObject: obj) { result.insert(p) }
        }
        return result
    }

    // MARK: - Diagnostics

    /// Diagnostic (`DIMMY_TAP_PROBE_ENUM=1`): log the current audio-active
    /// process set. Sanity-checks the enumeration helper without
    /// instantiating a tap. Useful before reproducing the Tahoe regression
    /// to confirm the HAL returns a non-empty list with the expected PIDs.
    static func runEnumerationProbe() {
        let selfPid = ProcessInfo.processInfo.processIdentifier
        let objs = audioActiveProcessObjects(excludingSelf: selfPid)
        NSLog("[EnumProbe] selfPid=%d active_audio_processes=%d", selfPid, objs.count)
        for obj in objs {
            let p = pid(forAudioObject: obj) ?? -1
            NSLog("[EnumProbe]   audioObjectID=%u pid=%d", obj, p)
        }
    }

    /// Env-gated probe (`DIMMY_TAP_PROBE=1`): start a tap for ~2 s, then log
    /// how many samples and what peak amplitude arrived before tearing down.
    /// Proves the capture chain (tap → aggregate → IO proc → samples) works
    /// on this machine without needing a live meeting. Never runs in a normal
    /// launch; handy when triaging "no system audio in the recording".
    static func runDiagnosticProbe() {
        final class Counters {
            var samples = 0
            var peak: Float = 0
        }
        let counters = Counters()
        let tap = SystemAudioProcessTap()
        tap.onSamples = { ptr, count, _ in
            counters.samples += count
            var p: Float = 0
            for i in 0..<count { p = max(p, abs(ptr[i])) }
            if p > counters.peak { counters.peak = p }
        }
        NSLog("[SystemAudio/probe] starting 2 s tap probe — play some audio now")
        switch tap.start() {
        case .failed:
            NSLog("[SystemAudio/probe] FAILED to start tap")
            return
        case .deferred:
            NSLog("[SystemAudio/probe] tap deferred (no audio source) — start audio and the rescan tick will promote")
        case .live:
            break
        }
        // Single-threaded diagnostic: the deferred stop runs strictly after
        // start, so opting these captures out of Sendable checking is safe.
        nonisolated(unsafe) let probeTap = tap
        nonisolated(unsafe) let probeCounters = counters
        DispatchQueue.global().asyncAfter(deadline: .now() + 2.0) {
            probeTap.stop()
            NSLog("[SystemAudio/probe] DONE samples=%d peak=%.4f",
                  probeCounters.samples, probeCounters.peak)
        }
    }

    /// UID of the current default output device — used as the aggregate's
    /// clock anchor. nil if there's no output device (rare; tap-only
    /// aggregate is then attempted).
    private static func defaultOutputDeviceUID() -> String? {
        var deviceAddress = AudioObjectPropertyAddress(
            mSelector: kAudioHardwarePropertyDefaultSystemOutputDevice,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain)
        var deviceID = AudioObjectID(kAudioObjectUnknown)
        var deviceSize = UInt32(MemoryLayout<AudioObjectID>.size)
        guard AudioObjectGetPropertyData(
            AudioObjectID(kAudioObjectSystemObject), &deviceAddress,
            0, nil, &deviceSize, &deviceID) == noErr,
            deviceID != AudioObjectID(kAudioObjectUnknown) else { return nil }

        var uidAddress = AudioObjectPropertyAddress(
            mSelector: kAudioDevicePropertyDeviceUID,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain)
        var uid: CFString?
        var uidSize = UInt32(MemoryLayout<CFString?>.size)
        let err = withUnsafeMutablePointer(to: &uid) {
            AudioObjectGetPropertyData(deviceID, &uidAddress, 0, nil, &uidSize, $0)
        }
        guard err == noErr, let uid else { return nil }
        return uid as String
    }

    /// Multi-config probe (`DIMMY_TAP_PROBE_MULTI=1`): try 6 aggregate
    /// configurations sequentially to isolate why `runDiagnosticProbe`
    /// returns samples=0 on Tahoe while Notion's tap captures audio.
    /// Each variant runs ~3 s; play audio throughout for ~25 s total.
    fileprivate struct ProbeVariant {
        let label: String
        let mutate: (inout [String: Any], String) -> Void
        let useDefaultOutput: Bool
        let driftComp: Bool
    }

    static func runMultiConfigProbe() {
        NSLog("[ProbeMulti] starting — play system audio NOW and keep it playing for ~30 s")
        let outDefSys = defaultOutputDeviceUID() ?? ""
        let outDef = defaultRegularOutputDeviceUID() ?? ""
        NSLog("[ProbeMulti] DefaultSystemOutput=%@ DefaultOutput=%@",
              outDefSys, outDef)

        let variants: [ProbeVariant] = [
            ProbeVariant(label: "V0_baseline",
                         mutate: { _, _ in },
                         useDefaultOutput: false,
                         driftComp: true),
            ProbeVariant(label: "V1_no_autostart",
                         mutate: { dict, _ in dict[kAudioAggregateDeviceTapAutoStartKey] = false },
                         useDefaultOutput: false,
                         driftComp: true),
            ProbeVariant(label: "V2_tap_only",
                         mutate: { dict, _ in
                             dict.removeValue(forKey: kAudioAggregateDeviceMainSubDeviceKey)
                             dict.removeValue(forKey: kAudioAggregateDeviceSubDeviceListKey)
                         },
                         useDefaultOutput: false,
                         driftComp: true),
            ProbeVariant(label: "V3_default_output",
                         mutate: { _, _ in },
                         useDefaultOutput: true,
                         driftComp: true),
            ProbeVariant(label: "V4_no_drift_comp",
                         mutate: { _, _ in },
                         useDefaultOutput: false,
                         driftComp: false),
            ProbeVariant(label: "V5_no_main_with_sublist",
                         mutate: { dict, _ in
                             dict.removeValue(forKey: kAudioAggregateDeviceMainSubDeviceKey)
                         },
                         useDefaultOutput: false,
                         driftComp: true),
        ]

        for variant in variants {
            tryVariant(variant)
            Thread.sleep(forTimeInterval: 0.4)
        }
        NSLog("[ProbeMulti] ALL DONE")
    }

    private static func tryVariant(_ variant: ProbeVariant) {
        let label = variant.label
        let outputUID = variant.useDefaultOutput
            ? (defaultRegularOutputDeviceUID() ?? defaultOutputDeviceUID())
            : defaultOutputDeviceUID()

        let description = CATapDescription(monoGlobalTapButExcludeProcesses: [])
        description.uuid = UUID()
        description.name = "Probe-\(label)"
        description.muteBehavior = .unmuted
        description.isPrivate = true
        description.isExclusive = false

        var tapID = AudioObjectID(kAudioObjectUnknown)
        var err = AudioHardwareCreateProcessTap(description, &tapID)
        guard err == noErr, tapID != AudioObjectID(kAudioObjectUnknown) else {
            NSLog("[ProbeMulti] %@ CreateTap FAIL err=%d", label, err)
            return
        }
        defer { AudioHardwareDestroyProcessTap(tapID) }

        guard let asbd = readTapFormat(tapID) else {
            NSLog("[ProbeMulti] %@ readTapFormat FAIL", label)
            return
        }
        let rate = asbd.mSampleRate > 0 ? Int32(asbd.mSampleRate) : 48_000

        let aggregateUID = UUID().uuidString
        var dict: [String: Any] = [
            kAudioAggregateDeviceNameKey: "Probe-Aggregate-\(label)",
            kAudioAggregateDeviceUIDKey: aggregateUID,
            kAudioAggregateDeviceIsPrivateKey: true,
            kAudioAggregateDeviceIsStackedKey: false,
            kAudioAggregateDeviceTapAutoStartKey: true,
            kAudioAggregateDeviceTapListKey: [[
                kAudioSubTapDriftCompensationKey: variant.driftComp,
                kAudioSubTapUIDKey: description.uuid.uuidString,
            ]],
        ]
        if let outputUID {
            dict[kAudioAggregateDeviceMainSubDeviceKey] = outputUID
            dict[kAudioAggregateDeviceSubDeviceListKey] = [[kAudioSubDeviceUIDKey: outputUID]]
        }
        variant.mutate(&dict, outputUID ?? "")

        var aggregateID = AudioObjectID(kAudioObjectUnknown)
        err = AudioHardwareCreateAggregateDevice(dict as CFDictionary, &aggregateID)
        guard err == noErr, aggregateID != AudioObjectID(kAudioObjectUnknown) else {
            NSLog("[ProbeMulti] %@ CreateAggregate FAIL err=%d", label, err)
            return
        }
        defer { AudioHardwareDestroyAggregateDevice(aggregateID) }

        // count input streams of aggregate (sanity)
        var streamsAddr = AudioObjectPropertyAddress(
            mSelector: kAudioDevicePropertyStreams,
            mScope: kAudioDevicePropertyScopeInput,
            mElement: kAudioObjectPropertyElementMain)
        var sz: UInt32 = 0
        AudioObjectGetPropertyDataSize(aggregateID, &streamsAddr, 0, nil, &sz)
        let nStreams = Int(sz) / MemoryLayout<AudioObjectID>.size

        final class C { var n = 0; var p: Float = 0 }
        let counter = C()
        nonisolated(unsafe) let unsafeCounter = counter

        var procID: AudioDeviceIOProcID?
        let queue = DispatchQueue(label: "probe.io.\(label)", qos: .userInteractive)
        err = AudioDeviceCreateIOProcIDWithBlock(&procID, aggregateID, queue) { _, inInput, _, _, _ in
            let abl = UnsafeMutableAudioBufferListPointer(UnsafeMutablePointer(mutating: inInput))
            guard let buf = abl.first, let data = buf.mData else { return }
            let count = Int(buf.mDataByteSize) / MemoryLayout<Float>.size
            unsafeCounter.n += count
            let fs = data.assumingMemoryBound(to: Float.self)
            var pk: Float = 0
            for i in 0..<count { pk = max(pk, abs(fs[i])) }
            if pk > unsafeCounter.p { unsafeCounter.p = pk }
        }
        guard err == noErr, let procID else {
            NSLog("[ProbeMulti] %@ CreateIOProc FAIL err=%d", label, err)
            return
        }
        defer {
            AudioDeviceStop(aggregateID, procID)
            AudioDeviceDestroyIOProcID(aggregateID, procID)
        }

        err = AudioDeviceStart(aggregateID, procID)
        if err != noErr {
            NSLog("[ProbeMulti] %@ AudioDeviceStart FAIL err=%d nStreams=%d",
                  label, err, nStreams)
            return
        }

        Thread.sleep(forTimeInterval: 3.0)

        NSLog("[ProbeMulti] %@ DONE outputUID=%@ nStreams=%d samples=%d peak=%.4f rate=%d",
              label, outputUID ?? "<none>", nStreams, counter.n, counter.p, rate)
    }

    /// Per-process probe (`DIMMY_TAP_PROBE_PID=<pid>`): build a tap that
    /// captures ONLY the audio output of the target pid (e.g. Chrome with
    /// a YouTube tab, Zoom call window). Runs ~5 s, logs sample count and
    /// peak amplitude. Disambiguates "Tahoe regression hits global tap only"
    /// (per-process works → switch to per-process path) from "Tahoe
    /// regression hits all taps" (per-process also peak=0 → no tap fix
    /// possible, must move to SCKit or another mechanism).
    static func runPerProcessProbe(pid: pid_t) {
        NSLog("[ProcessProbe] starting per-process tap for pid=%d — play audio in that app NOW", pid)
        guard let obj = processObject(for: pid) else {
            NSLog("[ProcessProbe] FAIL: pid=%d has no audio process object", pid)
            return
        }
        NSLog("[ProcessProbe] pid=%d → audioObjectID=%u", pid, obj)

        let description = CATapDescription(monoMixdownOfProcesses: [obj])
        description.uuid = UUID()
        description.name = "Probe-PerProcess-\(pid)"
        description.muteBehavior = .unmuted
        description.isPrivate = true
        description.isExclusive = false

        var tapID = AudioObjectID(kAudioObjectUnknown)
        var err = AudioHardwareCreateProcessTap(description, &tapID)
        guard err == noErr, tapID != AudioObjectID(kAudioObjectUnknown) else {
            NSLog("[ProcessProbe] CreateTap FAIL err=%d", err)
            return
        }
        defer { AudioHardwareDestroyProcessTap(tapID) }

        guard let asbd = readTapFormat(tapID) else {
            NSLog("[ProcessProbe] readTapFormat FAIL")
            return
        }
        let rate = asbd.mSampleRate > 0 ? Int32(asbd.mSampleRate) : 48_000

        let aggregateUID = UUID().uuidString
        let outputUID = defaultOutputDeviceUID()
        var dict: [String: Any] = [
            kAudioAggregateDeviceNameKey: "Probe-PerProcess-Agg-\(pid)",
            kAudioAggregateDeviceUIDKey: aggregateUID,
            kAudioAggregateDeviceIsPrivateKey: true,
            kAudioAggregateDeviceIsStackedKey: false,
            kAudioAggregateDeviceTapAutoStartKey: false,
            kAudioAggregateDeviceTapListKey: [[
                kAudioSubTapDriftCompensationKey: true,
                kAudioSubTapUIDKey: description.uuid.uuidString,
            ]],
        ]
        if let outputUID {
            dict[kAudioAggregateDeviceMainSubDeviceKey] = outputUID
            dict[kAudioAggregateDeviceSubDeviceListKey] = [[kAudioSubDeviceUIDKey: outputUID]]
        }

        var aggregateID = AudioObjectID(kAudioObjectUnknown)
        err = AudioHardwareCreateAggregateDevice(dict as CFDictionary, &aggregateID)
        guard err == noErr, aggregateID != AudioObjectID(kAudioObjectUnknown) else {
            NSLog("[ProcessProbe] CreateAggregate FAIL err=%d", err)
            return
        }
        defer { AudioHardwareDestroyAggregateDevice(aggregateID) }

        final class C { var n = 0; var p: Float = 0 }
        let counter = C()
        nonisolated(unsafe) let unsafeCounter = counter

        var procID: AudioDeviceIOProcID?
        let queue = DispatchQueue(label: "probe.pp", qos: .userInteractive)
        err = AudioDeviceCreateIOProcIDWithBlock(&procID, aggregateID, queue) { _, inInput, _, _, _ in
            let abl = UnsafeMutableAudioBufferListPointer(UnsafeMutablePointer(mutating: inInput))
            guard let buf = abl.first, let data = buf.mData else { return }
            let count = Int(buf.mDataByteSize) / MemoryLayout<Float>.size
            unsafeCounter.n += count
            let fs = data.assumingMemoryBound(to: Float.self)
            var pk: Float = 0
            for i in 0..<count { pk = max(pk, abs(fs[i])) }
            if pk > unsafeCounter.p { unsafeCounter.p = pk }
        }
        guard err == noErr, let procID else {
            NSLog("[ProcessProbe] CreateIOProc FAIL err=%d", err)
            return
        }
        defer {
            AudioDeviceStop(aggregateID, procID)
            AudioDeviceDestroyIOProcID(aggregateID, procID)
        }

        err = AudioDeviceStart(aggregateID, procID)
        if err != noErr {
            NSLog("[ProcessProbe] AudioDeviceStart FAIL err=%d", err)
            return
        }

        NSLog("[ProcessProbe] tap running 5 s — play audio NOW in pid=%d", pid)
        Thread.sleep(forTimeInterval: 5.0)

        NSLog("[ProcessProbe] DONE pid=%d audioObjectID=%u samples=%d peak=%.4f rate=%d",
              pid, obj, counter.n, counter.p, rate)
    }

    /// `kAudioHardwarePropertyDefaultOutputDevice` (vs DefaultSystemOutput).
    /// On macOS the two diverge when audio routes change (e.g. BT plug-in).
    private static func defaultRegularOutputDeviceUID() -> String? {
        var deviceAddress = AudioObjectPropertyAddress(
            mSelector: kAudioHardwarePropertyDefaultOutputDevice,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain)
        var deviceID = AudioObjectID(kAudioObjectUnknown)
        var deviceSize = UInt32(MemoryLayout<AudioObjectID>.size)
        guard AudioObjectGetPropertyData(
            AudioObjectID(kAudioObjectSystemObject), &deviceAddress,
            0, nil, &deviceSize, &deviceID) == noErr,
            deviceID != AudioObjectID(kAudioObjectUnknown) else { return nil }
        var uidAddress = AudioObjectPropertyAddress(
            mSelector: kAudioDevicePropertyDeviceUID,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain)
        var uid: CFString?
        var uidSize = UInt32(MemoryLayout<CFString?>.size)
        let err = withUnsafeMutablePointer(to: &uid) {
            AudioObjectGetPropertyData(deviceID, &uidAddress, 0, nil, &uidSize, $0)
        }
        guard err == noErr, let uid else { return nil }
        return uid as String
    }

    /// Read the tap's negotiated stream format (rate + channel count).
    private static func readTapFormat(_ tap: AudioObjectID) -> AudioStreamBasicDescription? {
        var address = AudioObjectPropertyAddress(
            mSelector: kAudioTapPropertyFormat,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain)
        var asbd = AudioStreamBasicDescription()
        var size = UInt32(MemoryLayout<AudioStreamBasicDescription>.size)
        let err = AudioObjectGetPropertyData(tap, &address, 0, nil, &size, &asbd)
        return err == noErr ? asbd : nil
    }
}

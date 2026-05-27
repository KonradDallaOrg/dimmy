import AudioToolbox
import CoreAudio
import Foundation
import os

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

    /// Latched true the first time the IO proc fires. When the audio-
    /// recording grant is missing the tap is created but its IO proc never
    /// runs, so this stays false — that's how the meeting tells "ungranted"
    /// apart from "granted but currently silent" (which still fires the proc
    /// with zero-amplitude buffers). Set on the realtime thread, read on main.
    private let receivedAudioFlag = OSAllocatedUnfairLock(initialState: false)
    var hasReceivedAudio: Bool { receivedAudioFlag.withLock { $0 } }

    private let ioQueue = DispatchQueue(
        label: "dimmy.systemaudio.tap.io", qos: .userInteractive)

    /// Create the tap + aggregate device + IO proc and start pulling audio.
    /// Returns false on any HAL failure so the caller can fall back to
    /// ScreenCaptureKit. Idempotent: a second call while running is a no-op.
    func start() -> Bool {
        guard !running else { return true }

        // Exclude Dimmy's own output from the global tap — mirror of
        // SCStream's `excludesCurrentProcessAudio = true`. If translation
        // fails we still build the tap (capturing self is harmless: AEC
        // already cancels our own playback, and Dimmy produces no audio
        // during a meeting).
        var excluded: [AudioObjectID] = []
        if let selfObj = Self.processObject(for: ProcessInfo.processInfo.processIdentifier) {
            excluded = [selfObj]
        }

        let description = CATapDescription(monoGlobalTapButExcludeProcesses: excluded)
        description.uuid = UUID()
        description.name = "Dimmy System Audio"
        description.muteBehavior = .unmuted
        description.isPrivate = true
        description.isExclusive = false

        var newTap = AudioObjectID(kAudioObjectUnknown)
        var err = AudioHardwareCreateProcessTap(description, &newTap)
        guard err == noErr, newTap != AudioObjectID(kAudioObjectUnknown) else {
            NSLog("[SystemAudio/tap] AudioHardwareCreateProcessTap failed: %d", err)
            return false
        }
        tapID = newTap

        guard let asbd = Self.readTapFormat(tapID) else {
            NSLog("[SystemAudio/tap] could not read tap stream format")
            teardown()
            return false
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
        NSLog("[SystemAudio/tap] tap format rate=%d ch=%d; clock anchor outputUID=%@",
              sampleRate, channelCount, outputUID ?? "<none>")
        let aggregateUID = UUID().uuidString
        var aggregateDescription: [String: Any] = [
            kAudioAggregateDeviceNameKey: "Dimmy System Audio Tap",
            kAudioAggregateDeviceUIDKey: aggregateUID,
            kAudioAggregateDeviceIsPrivateKey: true,
            kAudioAggregateDeviceIsStackedKey: false,
            kAudioAggregateDeviceTapAutoStartKey: true,
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
            return false
        }
        aggregateID = newAggregate

        // Capture into the realtime block by value — no self capture, no
        // allocation on the mono fast path.
        let handler = onSamples
        let rate = sampleRate
        let channels = channelCount
        let receivedFlag = receivedAudioFlag
        var newProcID: AudioDeviceIOProcID?
        err = AudioDeviceCreateIOProcIDWithBlock(&newProcID, aggregateID, ioQueue) {
            _, inInputData, _, _, _ in
            // Latch + detect the first fire so we can log "capture is live"
            // exactly once. A tap that's denied the audio-capture grant
            // never reaches this block, so the absence of this log line in
            // dimmy.log is the signature of a missing permission.
            let firstFire = receivedFlag.withLock { (state: inout Bool) -> Bool in
                let wasSet = state
                state = true
                return !wasSet
            }
            if firstFire {
                NSLog("[SystemAudio/tap] IO proc fired (first frame, %d samples) — capture is live",
                      Self.firstBufferSampleCount(inInputData))
            }
            Self.forward(inInputData, channels: channels, rate: rate, to: handler)
        }
        guard err == noErr, let procID = newProcID else {
            NSLog("[SystemAudio/tap] AudioDeviceCreateIOProcIDWithBlock failed: %d", err)
            teardown()
            return false
        }
        ioProcID = procID

        err = AudioDeviceStart(aggregateID, procID)
        guard err == noErr else {
            NSLog("[SystemAudio/tap] AudioDeviceStart failed: %d", err)
            teardown()
            return false
        }

        running = true
        NSLog("[SystemAudio/tap] started rate=%d ch=%d", rate, channels)
        return true
    }

    func stop() {
        guard running else { return }
        teardown()
        running = false
        NSLog("[SystemAudio/tap] stopped")
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

    private func teardown() {
        if let procID = ioProcID, aggregateID != AudioObjectID(kAudioObjectUnknown) {
            AudioDeviceStop(aggregateID, procID)
            AudioDeviceDestroyIOProcID(aggregateID, procID)
        }
        ioProcID = nil
        if aggregateID != AudioObjectID(kAudioObjectUnknown) {
            AudioHardwareDestroyAggregateDevice(aggregateID)
            aggregateID = AudioObjectID(kAudioObjectUnknown)
        }
        if tapID != AudioObjectID(kAudioObjectUnknown) {
            AudioHardwareDestroyProcessTap(tapID)
            tapID = AudioObjectID(kAudioObjectUnknown)
        }
    }

    // MARK: - HAL helpers

    /// Translate a pid to its CoreAudio process object id (needed for the
    /// tap's exclude list). nil if the pid has no audio process object.
    private static func processObject(for pid: pid_t) -> AudioObjectID? {
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

    // MARK: - Diagnostics

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
        guard tap.start() else {
            NSLog("[SystemAudio/probe] FAILED to start tap")
            return
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

import Foundation
import CoreGraphics

// MARK: - MeetingAmplitudeAGC
//
// Pure-data display-AGC + scrolling-history helpers shared between the
// meeting recording view and its tests. Mirrors the Win
// `MeetingWindow.xaml.cs` `OnAmpTick` formula so both platforms render
// the same shape from the same Rust FFI signal:
//
//     mic       = dimmy_get_amplitude()           // peak of last ~50 ms
//     system    = dimmy_get_loopback_amplitude()  // peak of last ~50 ms
//     display   = min(1, sqrt(raw) * 1.4)         // sqrt-curve + boost
//
// Sqrt-curve compresses high-end so a typical conversational range
// (~0.05 – 0.4) becomes a useful 0.31 – 0.83 visual band. The 1.4
// multiplier amplifies normal speech without saturating loud peaks.
//
// Lives in its own file (not nested in the ViewModel) so the unit
// tests can exercise it without dragging the @MainActor + Combine
// machinery + a live FFI in.

/// One sample slot in the meeting waveform. `mic` and `system` are both
/// post-AGC display levels in `[0, 1]`. When system audio isn't being
/// captured (mic-only mode, or no Mix devices), `system` is 0 across the
/// whole buffer and the view collapses to a single band.
struct MeetingAmplitudeSample: Equatable {
    let mic: CGFloat
    let system: CGFloat

    static let zero = MeetingAmplitudeSample(mic: 0, system: 0)
}

enum MeetingAmplitudeAGC {
    /// Map a raw FFI peak [0,1] to a display-AGC level [0,1]. Matches
    /// Win: `min(1, sqrt(raw) * 1.4)`. Filters NaN/Inf defensively —
    /// the FFI already clamps but a bad memory read mustn't crash the
    /// UI.
    static func displayLevel(_ raw: Float) -> CGFloat {
        guard raw.isFinite, raw > 0 else { return 0 }
        let boosted = sqrt(Double(raw)) * 1.4
        return CGFloat(min(1.0, max(0.0, boosted)))
    }

    /// Push a new (mic, system) sample into a fixed-capacity ring buffer.
    /// Drops the oldest entry when the buffer is full so the waveform
    /// scrolls left-to-right (newest on the right) — same shape as Win
    /// `_ampHistory.Enqueue` + `Dequeue`. Returns the updated buffer.
    static func push(_ buffer: [MeetingAmplitudeSample],
                     mic: CGFloat,
                     system: CGFloat,
                     capacity: Int) -> [MeetingAmplitudeSample] {
        precondition(capacity > 0, "MeetingAmplitudeAGC.push: capacity must be > 0")
        var next = buffer
        let sample = MeetingAmplitudeSample(
            mic: max(0, min(1, mic)),
            system: max(0, min(1, system))
        )
        next.append(sample)
        if next.count > capacity {
            next.removeFirst(next.count - capacity)
        }
        // Pad on the left with zero-samples so the bar count is stable
        // while the buffer is still warming up — otherwise the bars
        // jump around as the array grows.
        while next.count < capacity {
            next.insert(.zero, at: 0)
        }
        assert(next.count == capacity, "post: buffer length must equal capacity")
        return next
    }
}

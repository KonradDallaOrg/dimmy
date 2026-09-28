import SwiftUI

// MARK: - MeetingSpeakers
//
// The speakers a diarized meeting was split into, as the core writes them to
// `speakers.json` (see core/src/diarize.rs). Read-only here: a rename goes
// through `dimmy_meeting_rename_speaker`, which rewrites both this file and
// the labels in `transcripts.txt`. Mirror of Win Helpers/MeetingSpeakers.cs.

struct MeetingSpeaker: Identifiable, Equatable {
    let id: String
    let name: String
    let band: String
    let talkSecs: Double
    let segments: [(start: Double, end: Double)]
    /// Palette slot = position in the file, so the transcript label, the chip
    /// and the waveform lane always agree.
    let colorIndex: Int

    static func == (a: MeetingSpeaker, b: MeetingSpeaker) -> Bool {
        a.id == b.id && a.name == b.name && a.colorIndex == b.colorIndex
            && a.segments.count == b.segments.count
    }
}

enum MeetingSpeakers {
    static let fileName = "speakers.json"

    /// Speakers in id order. Empty when the meeting was not diarized or the
    /// file is unreadable.
    static func load(dir: String?) -> [MeetingSpeaker] {
        guard let dir, !dir.isEmpty else { return [] }
        let url = URL(fileURLWithPath: dir).appendingPathComponent(fileName)
        guard let data = try? Data(contentsOf: url),
              let arr = (try? JSONSerialization.jsonObject(with: data)) as? [[String: Any]]
        else { return [] }
        var out: [MeetingSpeaker] = []
        for s in arr {
            guard let id = s["id"] as? String, let name = s["name"] as? String else { return [] }
            let segs = (s["segments"] as? [[Double]] ?? [])
                .filter { $0.count == 2 }
                .map { (start: $0[0], end: $0[1]) }
            out.append(MeetingSpeaker(
                id: id,
                name: name,
                band: s["band"] as? String ?? "",
                talkSecs: s["talk_secs"] as? Double ?? 0,
                segments: segs,
                colorIndex: out.count))
        }
        return out
    }

    /// Label (case-insensitive) → palette slot, for the transcript.
    static func colorsByName(_ speakers: [MeetingSpeaker]) -> [String: Int] {
        var map: [String: Int] = [:]
        for s in speakers { map[s.name.lowercased()] = s.colorIndex }
        return map
    }

    static func isTrack(_ label: String) -> Bool {
        let l = label.lowercased()
        return l == "mic" || l == "system"
    }

    /// A stable slot for a label speakers.json does not know.
    static func stableIndex(_ name: String) -> Int {
        var h: Int32 = 0
        for u in name.lowercased().unicodeScalars { h = h &* 31 &+ Int32(truncatingIfNeeded: u.value) }
        return Int(abs(Int(h) % palette.count))
    }

    // Eight hues far apart from each other and from the mic mint / system
    // violet track colours. Light shade reads on dark, deep one on light.
    // Same values as Windows so a meeting looks the same on both.
    private static let palette: [(dark: UInt32, light: UInt32)] = [
        (0x8AB4FF, 0x1F4FB3), // blue
        (0xFFC46B, 0x8A5A00), // amber
        (0xFF8FB8, 0xA3285A), // pink
        (0x6EE0E6, 0x0E6B70), // teal
        (0xFFA07A, 0xA84415), // orange
        (0xB8E07A, 0x4E6B12), // lime
        (0xE0B8FF, 0x6B2FA8), // orchid
        (0xE0B8A0, 0x7A4A30), // clay
    ]

    static func color(_ index: Int, dark: Bool) -> Color {
        let n = palette.count
        let entry = palette[((index % n) + n) % n]
        let v = dark ? entry.dark : entry.light
        return Color(red: Double((v >> 16) & 0xFF) / 255,
                     green: Double((v >> 8) & 0xFF) / 255,
                     blue: Double(v & 0xFF) / 255)
    }

    /// "1:37" / "1:02:05" — talk time on a chip.
    static func formatTalkTime(_ secs: Double) -> String {
        let t = Int(max(0, secs).rounded())
        return t >= 3600
            ? String(format: "%d:%02d:%02d", t / 3600, (t % 3600) / 60, t % 60)
            : String(format: "%d:%02d", t / 60, t % 60)
    }

    /// Message for a failed rename, by core rc.
    static func renameError(_ rc: Int32) -> String {
        switch rc {
        case -4: return "Use 1–40 characters, no brackets, and not \"mic\" or \"system\"."
        case -5: return "Another speaker already has this name."
        default: return "Could not rename this speaker."
        }
    }
}

// MARK: - DiarizationService
//
// Speaker labels for a meeting that just stopped. The live transcript is
// written per track in 15-30 s chunks and cannot be split by voice; when the
// user turned speaker labels on, the whole meeting is transcribed again from
// the saved audio with diarization (core `dimmy_meeting_retranscribe`), so
// the recap that follows already knows who said what.
// Mirror of Win Services/DiarizationService.cs.

enum DiarizationService {
    /// Local STT only: re-transcribing with a cloud provider would upload the
    /// whole meeting a second time. There, labels come from an explicit
    /// "Regenerate transcript" instead.
    static func enabled() -> Bool {
        guard let cfg = DimmyCore.shared.getConfig() else { return false }
        return (cfg["diarization_enabled"] as? Bool) == true
            && (cfg["stt_mode"] as? String) == "local"
            && DimmyCore.shared.diarizationModelPresent()
    }

    /// The speaker-labelled transcript of `dir`, or `liveTranscript` unchanged
    /// when labels are off or the pass fails — a failed diarization must never
    /// cost the user the transcript or the recap they would have had without
    /// it. BLOCKING — call off the main thread.
    static func relabelIfEnabled(dir: String, liveTranscript: String) -> String {
        guard !dir.isEmpty, enabled() else { return liveTranscript }
        let t0 = Date()
        let result = DimmyCore.shared.meetingRetranscribe(dir: dir)
        let labelled = (try? result.get())?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        dimmyHostLog(String(format: "[Meeting] diarized relabel %@ in %.1fs",
                            labelled.isEmpty ? "failed" : "ok", Date().timeIntervalSince(t0)))
        return labelled.isEmpty ? liveTranscript : labelled
    }
}

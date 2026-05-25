import Foundation

// MARK: - FileLoadToMeetingService
//
// Mac mirror of Win `Services/FileLoadToMeetingService.cs`. Bridges the
// File Load card's transcript to the Meeting recap pipeline.
//
// Given a successful local transcript + the source WAV path, this:
//   1. Mints a synthetic meeting directory under
//      `<configDir>/meetings/<timestamp>-<uuid>` — the same root the
//      live recorder writes to, so Meeting → History picks it up
//      automatically.
//   2. Writes `transcripts.txt` in the `[ELAPSED_MS ms] [LABEL] text`
//      grammar `MeetingPostProcessService.buildStructuredRecapPrompt`
//      expects — single line `[0 ms] [file] <text>` for a file-load.
//   3. Writes `meta.json` with `title`/`started_at`/`duration_secs` so
//      the History row renders the right subtitle + duration.
//   4. Best-effort copies the source WAV to `audio.wav` so the Done
//      view's audio scrubber works. Failure here doesn't abort the
//      recap — disk-full / permission errors just degrade to "recap
//      without playback".
//   5. Hands off to `MeetingPostProcessService.runRecap(dir:transcript:)`
//      which calls the LLM and persists `recap.md` + `actions.json`.
//
// Single-writer rule preserved: only this service writes to the
// meeting dir on the file-load path; Rust still owns config.json + the
// live-meeting writers.

enum FileLoadToMeetingService {

    struct Outcome {
        let dir: String
        let recapMarkdown: String?
        let error: String?
        var success: Bool { error == nil }
    }

    /// Run the full file-load → meeting pipeline. Blocking — call
    /// from a background thread (the LLM step inside
    /// `MeetingPostProcessService.runRecap` can take 10–60 s).
    ///
    /// `notionAutoSend` MUST be captured on the caller's main-actor
    /// thread before dispatch — Swift 6 strict concurrency rejects
    /// any read of `AppState.shared.notionAutoSend` (main-actor
    /// isolated) from a `DispatchQueue.global` closure as a hard
    /// error.
    static func run(sourceWavPath: String, transcript: String, notionAutoSend: Bool) -> Outcome {
        let trimmedTranscript = transcript.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmedTranscript.isEmpty else {
            return Outcome(dir: "", recapMarkdown: nil, error: "empty transcript")
        }

        // Effective meetings dir (honours the user's meeting_storage_path
        // override) — never re-derive configDir/meetings here.
        guard let meetingsRoot = DimmyCore.shared.meetingsDirURL else {
            return Outcome(dir: "", recapMarkdown: nil, error: "meetings dir unavailable")
        }
        do {
            try FileManager.default.createDirectory(at: meetingsRoot, withIntermediateDirectories: true)
        } catch {
            return Outcome(dir: "", recapMarkdown: nil,
                           error: "could not create meetings root: \(error.localizedDescription)")
        }

        // Dir name shape mirrors the live worker's so the History
        // sort-by-modified-time keeps file-load entries inline with
        // recorded ones.
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withYear, .withMonth, .withDay, .withTime]
        let stamp = Date().formatted(.iso8601.year().month().day().time(includingFractionalSeconds: false))
            .replacingOccurrences(of: ":", with: "")
            .replacingOccurrences(of: "-", with: "")
        let dirName = "\(stamp)-\(UUID().uuidString.replacingOccurrences(of: "-", with: ""))"
        let dir = meetingsRoot.appendingPathComponent(dirName, isDirectory: true)
        do {
            try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        } catch {
            return Outcome(dir: dir.path, recapMarkdown: nil,
                           error: "could not create meeting dir: \(error.localizedDescription)")
        }

        // transcripts.txt — the recap prompt expects the
        // `[ELAPSED_MS ms] [LABEL] text` grammar (see
        // MeetingPostProcessService.buildStructuredRecapPrompt). A
        // file-load is one big line — the LLM still produces a useful
        // recap, just without intra-meeting time anchors.
        let transcriptsURL = dir.appendingPathComponent("transcripts.txt")
        let transcriptLine = "[0 ms] [file] \(trimmedTranscript)\n"
        do {
            try transcriptLine.write(to: transcriptsURL, atomically: true, encoding: .utf8)
        } catch {
            return Outcome(dir: dir.path, recapMarkdown: nil,
                           error: "could not write transcripts.txt: \(error.localizedDescription)")
        }

        // audio.wav copy + duration probe. Best-effort.
        var durationSecs: Double = 0
        if !sourceWavPath.isEmpty,
           FileManager.default.fileExists(atPath: sourceWavPath) {
            let dest = dir.appendingPathComponent("audio.wav")
            do {
                try? FileManager.default.removeItem(at: dest)
                try FileManager.default.copyItem(at: URL(fileURLWithPath: sourceWavPath), to: dest)
                durationSecs = WavPeaks.readDurationSecs(path: dest.path)
            } catch {
                NSLog("[FileLoadToMeeting] audio.wav copy failed: \(error.localizedDescription)")
            }
        }

        // meta.json — Meeting → History reads `started_at` (string)
        // + `duration_secs` (Double) + `title` (String). Title comes
        // from the source filename so the user can tell file-load
        // entries apart from live recordings at a glance.
        let baseName: String
        if sourceWavPath.isEmpty {
            baseName = "Imported audio"
        } else {
            baseName = URL(fileURLWithPath: sourceWavPath).deletingPathExtension().lastPathComponent
        }
        let metaURL = dir.appendingPathComponent("meta.json")
        let dateFmt = DateFormatter()
        dateFmt.dateFormat = "yyyy-MM-dd HH:mm"
        let meta: [String: Any] = [
            "title": "File: \(baseName)",
            "started_at": dateFmt.string(from: Date()),
            "duration_secs": durationSecs,
            "origin": "file-load",
        ]
        if let json = try? JSONSerialization.data(
            withJSONObject: meta, options: [.prettyPrinted]) {
            try? json.write(to: metaURL)
        }

        NSLog("[FileLoadToMeeting] dir=\(dir.path) transcript=\(trimmedTranscript.count)c duration=\(durationSecs)s")

        // Tag the synthetic meeting so the funnel can distinguish
        // "live recorded" from "imported audio". Win emits this
        // before kicking off the recap; we mirror so dashboards
        // are platform-agnostic.
        DimmyCore.shared.trackEvent("meeting.imported_from_file")

        // Hand off to the shared recap pipeline. `notionAutoSend`
        // was captured on the caller's main thread (see docstring).
        // We still gate it on `notionHasToken` here because the
        // token state is owned by DimmyCore and safe to read from
        // any thread (it's a thin FFI getter, not @MainActor).
        let effectiveAutoSend = notionAutoSend && DimmyCore.shared.notionHasToken
        let recapResult = MeetingPostProcessService.runRecap(
            dir: dir.path,
            transcript: trimmedTranscript,
            modelOverride: nil,
            notionAutoSend: effectiveAutoSend
        )
        switch recapResult {
        case .success(let res):
            NSLog("[FileLoadToMeeting] recap ok at \(dir.path)")
            return Outcome(dir: dir.path, recapMarkdown: res.recapMarkdown, error: nil)
        case .failure(let err):
            return Outcome(dir: dir.path, recapMarkdown: nil, error: "\(err)")
        }
    }
}

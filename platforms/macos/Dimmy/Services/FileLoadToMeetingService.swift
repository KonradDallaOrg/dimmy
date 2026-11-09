import AVFoundation
import Foundation

// MARK: - FileLoadToMeetingService
//
// Mac mirror of Win `Services/FileLoadToMeetingService.cs`. Bridges the
// File Load card's transcript to the Meeting recap pipeline.
//
// Given a successful transcript + the source audio path, the core makes the
// meeting (`dimmy_meeting_import`: folder, transcripts.txt, audio copy,
// meta.json, the same shape as a live meeting), then
// `MeetingPostProcessService.runRecap(dir:transcript:)` runs the recap. Only
// the core writes the meeting folder.

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

        // Probed on the source: the core copies it, keeping its own
        // container (core `meeting::import_from_file`).
        var durationSecs: Double = 0
        if !sourceWavPath.isEmpty, FileManager.default.fileExists(atPath: sourceWavPath) {
            let src = URL(fileURLWithPath: sourceWavPath)
            durationSecs = src.pathExtension.lowercased() == "wav"
                ? WavPeaks.readDurationSecs(path: src.path)
                : ((try? AVAudioPlayer(contentsOf: src))?.duration ?? 0)
        }
        if !durationSecs.isFinite || durationSecs < 0 { durationSecs = 0 }

        // The core makes the meeting in the shape a live one has: this
        // service used to write it all, with its own folder names and a
        // local-time `started_at` string where every other meeting has Unix
        // seconds. Honours the meeting_storage_path override like a live one.
        guard let dirPath = DimmyCore.shared.meetingImport(
            source: sourceWavPath, transcript: trimmedTranscript, durationSecs: durationSecs)
        else {
            return Outcome(dir: "", recapMarkdown: nil, error: "could not create the meeting")
        }
        let dir = URL(fileURLWithPath: dirPath, isDirectory: true)

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
        // Same notice the pill-stop recap posts: an open meeting window loads
        // the new meeting instead of staying on the stream it was showing.
        let recapOK = (try? recapResult.get()) != nil
        DispatchQueue.main.async {
            NotificationCenter.default.post(name: .meetingRecapSaved, object: nil,
                                            userInfo: ["dir": dir.path, "success": recapOK])
        }
        switch recapResult {
        case .success(let res):
            NSLog("[FileLoadToMeeting] recap ok at \(dir.path)")
            return Outcome(dir: dir.path, recapMarkdown: res.recapMarkdown, error: nil)
        case .failure(let err):
            return Outcome(dir: dir.path, recapMarkdown: nil, error: "\(err)")
        }
    }
}

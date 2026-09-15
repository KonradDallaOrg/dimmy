import Foundation

// MARK: - TelegramService
//
// Host-side orchestrator for the Telegram "Saved Messages audio inbox".
// Mac mirror of Win `Services/TelegramService.cs`. The Rust worker
// (core/src/telegram.rs) owns login + watching Saved Messages + download
// and emits four events (telegram_state / _pending / _audio / _error),
// dispatched into `DimmyCore.handleEvent`. This class owns the host
// policy on top of those events:
//
//   • start the worker at launch when `telegram_enabled` is set,
//   • on a new audio (`telegram_pending`): auto-process, or queue a
//     one-at-a-time nudge asking "transcribe + recap?",
//   • on download-ready (`telegram_audio`): run the SAME file-load
//     transcribe + recap pipeline the "Transcribe a file" card uses
//     (`DimmyCore.transcribeFile` -> `FileLoadToMeetingService.run`),
//     then `dimmy_telegram_mark_processed`,
//   • toast the outcome (ready / transcribed-no-recap / failed).
//
// Single-writer rule preserved: config flips go through
// `dimmy_set_config_json`; only the meeting dir is written by the
// shared file-load pipeline.
@MainActor
final class TelegramService {
    static let shared = TelegramService()

    private struct PendingItem {
        let msgId: Int32
        let filename: String
    }

    private var autoProcess: Bool = false
    private var nudgeQueue: [PendingItem] = []
    private var showingNudge: Bool = false
    /// True while this service is the one driving the pill and the menu-bar
    /// icon. Work arriving from Telegram used to run for minutes with the UI
    /// showing nothing at all, which read as a frozen app.
    private var drivingPill: Bool = false

    private init() {}

    // MARK: - Lifecycle

    /// Called once at launch. Starts the Rust worker when the feature is
    /// enabled in config; the worker restores its saved session and
    /// emits `telegram_state` so the Settings card lands on the right
    /// state without polling.
    func start(appState: AppState) {
        autoProcess = appState.telegramAutoProcess
        if appState.telegramEnabled {
            DimmyCore.shared.telegramSetEnabled(true)
        }
        refreshState(appState: appState)
    }

    /// Pull a fresh status snapshot into AppState (used on launch and
    /// when a Settings view opens, so the UI is correct before the next
    /// event arrives).
    func refreshState(appState: AppState) {
        guard let s = DimmyCore.shared.telegramStatus() else { return }
        appState.telegramCompiled = s["compiled"] as? Bool ?? false
        appState.telegramHasCredentials = s["has_credentials"] as? Bool ?? false
        appState.telegramPhase = s["phase"] as? String ?? "disabled"
        appState.telegramAccount = s["account"] as? String ?? ""
        appState.telegramPending = s["pending"] as? Int ?? 0
    }

    /// Keep the ask-vs-auto policy in sync when the user flips the
    /// Settings toggle.
    func setAutoProcess(_ value: Bool) {
        autoProcess = value
    }

    // MARK: - Event handlers (called from DimmyCore.handleEvent)

    /// A new audio message arrived in Saved Messages. Auto-process, or
    /// queue a nudge.
    func onPending(msgId: Int32, filename: String, sizeBytes: Int64, isBacklog: Bool) {
        if autoProcess {
            // Auto-process shows no nudge, so this was the only moment with
            // nothing on screen at all: the download plus transcription plus
            // recap can run for many minutes.
            DictToastWindow.show(
                kind: .alreadyPresent,
                title: "Telegram audio received",
                body: "Transcribing \(Self.displayName(filename)) now.")
            _ = DimmyCore.shared.telegramProcess(msgId: msgId)
            // Answer where the audio came from: whoever sent it is holding a
            // phone, not watching this Mac.
            DimmyCore.shared.telegramReply(
                msgId: msgId,
                text: "Dimmy got it. Transcribing now, I will reply here when the recap is ready.")
            return
        }
        nudgeQueue.append(PendingItem(msgId: msgId, filename: filename))
        showNextNudgeIfIdle()
    }

    /// The worker finished downloading a message's media to `path`. Run
    /// transcribe + recap on a background thread, then mark it processed.
    func onAudioReady(msgId: Int32, path: String, filename: String) {
        // Capture the main-actor-isolated flag before hopping off-main:
        // Swift 6 strict concurrency rejects reading it inside a global
        // closure (see FileLoadToMeetingService.run docstring).
        let notionAutoSend = AppState.shared.notionAutoSend
        let displayName = Self.displayName(filename)
        beginPillActivity(.transcribing)

        DispatchQueue.global(qos: .userInitiated).async {
            let result = DimmyCore.shared.transcribeFile(at: path)
            switch result {
            case .success(let transcript):
                // The transcript is in; what follows is the recap, and the pill
                // says so itself ("Recap...", from runRecap's count). Holding
                // .processing here gave the same work a different label from
                // a meeting recap.
                Task { @MainActor in TelegramService.shared.endPillActivity() }
                let outcome = FileLoadToMeetingService.run(
                    sourceWavPath: path,
                    transcript: transcript,
                    notionAutoSend: notionAutoSend)
                let recapReply = Self.recapReply(dir: outcome.dir)
                Task { @MainActor in
                    // transcribe_file already consumed the media, so the
                    // message is handled whether or not the recap step
                    // succeeded — mark it so it is not re-offered.
                    _ = DimmyCore.shared.telegramMarkProcessed(msgId: msgId)
                    TelegramService.shared.endPillActivity()
                    if outcome.success, (outcome.recapMarkdown?.isEmpty == false) {
                        DictToastWindow.show(
                            kind: .added,
                            title: "Telegram audio ready",
                            body: "\(displayName) was transcribed and recapped.")
                        DimmyCore.shared.telegramReply(
                            msgId: msgId,
                            text: recapReply ?? "Transcribed and recapped. The recap is in Dimmy.")
                    } else {
                        DictToastWindow.show(
                            kind: .alreadyPresent,
                            title: "Telegram audio transcribed",
                            body: "\(displayName) was saved to History. A recap could not be generated — check your LLM setup.")
                        DimmyCore.shared.telegramReply(
                            msgId: msgId,
                            text: "Transcribed and saved to Dimmy's history. No recap was produced: check the recap model in Settings.")
                    }
                    TelegramService.shared.showNextNudgeIfIdle()
                }
            case .failure:
                Task { @MainActor in
                    // Leave it un-marked so it stays in the inbox to retry.
                    TelegramService.shared.endPillActivity()
                    DictToastWindow.show(
                        kind: .error,
                        title: "Telegram audio failed",
                        body: "Could not transcribe \(displayName). It stays in your Telegram inbox to retry.")
                    DimmyCore.shared.telegramReply(
                        msgId: msgId,
                        text: "Dimmy could not transcribe this one. It stays in the inbox, so sending it again is not needed.")
                    TelegramService.shared.showNextNudgeIfIdle()
                }
            }
        }
    }

    // MARK: - Pill + menu-bar activity

    /// Light up the pill and the menu-bar icon, but never take them from a
    /// dictation or a meeting: those own the same state and their own stop
    /// paths would then be fighting this one.
    private func beginPillActivity(_ state: RecordingState) {
        if !drivingPill, AppState.shared.recordingState != .idle { return }
        drivingPill = true
        AppState.shared.recordingState = state
    }

    /// Back to idle, unless something else took the pill over meanwhile.
    private func endPillActivity() {
        guard drivingPill else { return }
        drivingPill = false
        let current = AppState.shared.recordingState
        if current == .transcribing || current == .processing {
            AppState.shared.recordingState = .idle
        }
    }

    // MARK: - Telegram reply text

    static func displayName(_ filename: String) -> String {
        filename.isEmpty ? "the voice note" : filename
    }

    /// The finished-work reply, from the `recap.md` the pipeline just wrote.
    static func recapReply(dir: String) -> String? {
        guard !dir.isEmpty,
              let markdown = try? String(
                contentsOf: URL(fileURLWithPath: dir).appendingPathComponent("recap.md"),
                encoding: .utf8)
        else { return nil }
        return replyText(fromRecapMarkdown: markdown)
    }

    /// Title plus the recap's own one-paragraph summary. nil when there is
    /// nothing worth quoting, so the caller falls back to a plain
    /// confirmation rather than sending an empty message. Pure on purpose:
    /// whoever sent the voice note is on a phone, away from this Mac, and
    /// this text is the only thing they see. Win parity:
    /// `Helpers/TelegramReplyText.cs`.
    static func replyText(fromRecapMarkdown markdown: String) -> String? {
        var title = ""
        var summary: [String] = []
        var inSummary = false
        for line in markdown.split(separator: "\n", omittingEmptySubsequences: false) {
            let text = line.trimmingCharacters(in: .whitespaces)
            if title.isEmpty, text.hasPrefix("# ") {
                title = String(text.dropFirst(2)).trimmingCharacters(in: .whitespaces)
                continue
            }
            if text.hasPrefix("## ") {
                // Stop at the heading after the summary. Matched by name
                // because the recap is written in the meeting's language and
                // only this marker stays English.
                if inSummary { break }
                let heading = text.lowercased()
                inSummary = heading.contains("tl;dr") || heading.contains("tldr")
                continue
            }
            // The AI-marking comment sits on line 2 of every recap and must
            // never be quoted back at the user.
            if inSummary, !text.isEmpty, !text.hasPrefix("<!--") {
                summary.append(text)
            }
        }
        let body = summary.joined(separator: " ")
        if title.isEmpty && body.isEmpty { return nil }
        if body.isEmpty { return title }
        return title.isEmpty ? body : "\(title)\n\n\(body)"
    }

    // MARK: - Nudge queue (one prompt at a time)

    func showNextNudgeIfIdle() {
        guard !showingNudge, let item = nudgeQueue.first else { return }
        showingNudge = true
        TelegramNudgeWindowController.shared.show(msgId: item.msgId, filename: item.filename)
    }

    func acceptNudge(msgId: Int32) {
        dropFromQueue(msgId)
        showingNudge = false
        _ = DimmyCore.shared.telegramProcess(msgId: msgId)  // -> telegram_audio -> onAudioReady
        DimmyCore.shared.telegramReply(
            msgId: msgId,
            text: "Dimmy got it. Transcribing now, I will reply here when the recap is ready.")
        showNextNudgeIfIdle()
    }

    func dismissNudge(msgId: Int32) {
        dropFromQueue(msgId)
        showingNudge = false
        _ = DimmyCore.shared.telegramDismiss(msgId: msgId)
        showNextNudgeIfIdle()
    }

    private func dropFromQueue(_ msgId: Int32) {
        nudgeQueue.removeAll { $0.msgId == msgId }
    }
}

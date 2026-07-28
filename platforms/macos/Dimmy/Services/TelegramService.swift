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
            _ = DimmyCore.shared.telegramProcess(msgId: msgId)
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
        let displayName = filename.isEmpty ? "Voice note" : filename

        DispatchQueue.global(qos: .userInitiated).async {
            let result = DimmyCore.shared.transcribeFile(at: path)
            switch result {
            case .success(let transcript):
                let outcome = FileLoadToMeetingService.run(
                    sourceWavPath: path,
                    transcript: transcript,
                    notionAutoSend: notionAutoSend)
                Task { @MainActor in
                    // transcribe_file already consumed the media, so the
                    // message is handled whether or not the recap step
                    // succeeded — mark it so it is not re-offered.
                    _ = DimmyCore.shared.telegramMarkProcessed(msgId: msgId)
                    if outcome.success, (outcome.recapMarkdown?.isEmpty == false) {
                        DictToastWindow.show(
                            kind: .added,
                            title: "Telegram audio ready",
                            body: "\(displayName) was transcribed and recapped.")
                    } else {
                        DictToastWindow.show(
                            kind: .alreadyPresent,
                            title: "Telegram audio transcribed",
                            body: "\(displayName) was saved to History. A recap could not be generated — check your LLM setup.")
                    }
                    TelegramService.shared.showNextNudgeIfIdle()
                }
            case .failure:
                Task { @MainActor in
                    // Leave it un-marked so it stays in the inbox to retry.
                    DictToastWindow.show(
                        kind: .error,
                        title: "Telegram audio failed",
                        body: "Could not transcribe \(displayName). It stays in your Telegram inbox to retry.")
                    TelegramService.shared.showNextNudgeIfIdle()
                }
            }
        }
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

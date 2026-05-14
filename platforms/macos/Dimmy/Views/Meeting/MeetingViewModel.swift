import Foundation
import SwiftUI
import AppKit
import Combine

// MARK: - MeetingViewModel
//
// State machine + data layer for the meeting window. Mirror of the Win
// MeetingWindow code-behind state model:
//
//   Idle        → user hasn't started a meeting and isn't browsing one
//   Recording   → meeting in flight; live transcript streams in
//   Processing  → meeting just stopped; LLM recap in flight
//   Done        → showing a completed meeting (just-finished OR a
//                 sidebar selection of a past meeting)
//
// Recording state is OWNED BY THE RUST CORE (`MEETING` static). This
// VM is a thin mirror over the FFI — it does NOT cache "is recording"
// independently of the core. Window close / reopen MUST re-sync via
// `dimmy_meeting_is_active()` and `dimmy_meeting_is_paused()` in
// `attachToInflightIfAny`.

@MainActor
final class MeetingViewModel: ObservableObject {
    enum Phase: Equatable {
        case idle
        case recording
        case processing
        case done
    }

    // ── Top-level state ────────────────────────────────────────────
    @Published var phase: Phase = .idle
    @Published var generateRecap: Bool = true
    @Published var statusLabel: String = "Ready"
    @Published var subStatusLabel: String = "Click Start to begin a meeting recording"
    @Published var titlebarTitle: String = "New meeting"

    // ── Recording state ────────────────────────────────────────────
    @Published var transcript: String = ""
    @Published var timerLabel: String = "00:00:00"
    @Published var chunkSummary: String = ""
    @Published var isPaused: Bool = false
    static let liveAmplitudeBarCount: Int = 56
    @Published var liveAmplitudeBars: [MeetingAmplitudeSample] =
        Array(repeating: .zero, count: MeetingViewModel.liveAmplitudeBarCount)

    // ── Sidebar ────────────────────────────────────────────────────
    @Published var historyRows: [MeetingHistoryRow] = []
    @Published var historySearch: String = ""
    @Published var selectedDir: String?  // nil while looking at active recording

    // ── Done state ─────────────────────────────────────────────────
    @Published var doneTitle: String = "Meeting"
    @Published var doneMeta: String = ""
    @Published var doneSections: [String: String] = [:]
    @Published var doneRawTranscript: String = ""
    @Published var doneAudioURL: URL?
    @Published var browsingPastMeeting: Bool = false  // true when the user clicked a sidebar row while recording is in flight
    @Published var processingStep: ProcessingStep = .saving

    // ── Toast (blocked-during-recording warning) ───────────────────
    @Published var toastMessage: String?

    /// Combine bag for the live-transcript pipe from AppState.
    /// DimmyCore.handleEvent writes every `meeting_chunk` event's
    /// `line` into `AppState.meetingLiveTranscript`; we mirror that
    /// onto `self.transcript` so the recording view binds to a
    /// single property on its own view-model (Combine-friendly,
    /// no AppState reach-through in the View).
    private var liveTranscriptBag = Set<AnyCancellable>()

    init() {
        // Replaces the absent 2 s file polling that Win used to run
        // on transcripts.txt — purely event-driven. Subscription is
        // permanent (no cancel/re-subscribe per meeting) because
        // AppState.meetingLiveTranscript is reset to "" on each
        // start() / stopAndProcess(); we just mirror.
        AppState.shared.$meetingLiveTranscript
            .receive(on: DispatchQueue.main)
            .sink { [weak self] value in
                self?.transcript = value
            }
            .store(in: &liveTranscriptBag)
        AppState.shared.$meetingChunkCount
            .receive(on: DispatchQueue.main)
            .sink { [weak self] count in
                guard let self else { return }
                self.chunkSummary = count > 0 ? "\(count) chunks" : ""
            }
            .store(in: &liveTranscriptBag)
    }

    // ── Errors ─────────────────────────────────────────────────────
    @Published var lastError: String?

    // MARK: - Internals

    enum ProcessingStep {
        case saving
        case generatingRecap
        case extractingActions
    }

    private var startedAt: Date?
    private var pollTimer: Timer?
    private var amplitudeTimer: Timer?
    private var sessionId: String = ""
    private var isWorking: Bool = false
    private var activeMeetingDir: String = ""
    private var toastDismissTask: Task<Void, Never>?

    var dotColor: Color {
        switch phase {
        case .idle: return .gray
        case .recording: return isPaused ? .orange : .red
        case .processing: return .blue
        case .done: return .green
        }
    }

    /// Whether ANY sample in the live waveform buffer has a non-zero
    /// system-audio level. Used to gate the second band in the recording
    /// view: mic-only meetings stay single-band rather than rendering an
    /// always-empty lower half.
    var systemAudioActive: Bool {
        liveAmplitudeBars.contains { $0.system > 0.001 }
    }

    // MARK: - Lifecycle: window opens / re-opens

    /// Called by MeetingWindowController whenever the window is shown.
    /// Re-attaches to an in-flight meeting if one is running (e.g. user
    /// closed the window mid-recording, then reopened it). Falls back
    /// to history-load + Idle when nothing is in flight.
    func onWindowShown() {
        loadHistory()
        if DimmyCore.shared.meetingIsActive {
            attachToInflightMeeting()
        } else if phase == .idle {
            // Nothing to attach to — keep current state. If we just
            // came back from a Done (selected past meeting), don't
            // reset; the Idle hero is already correct otherwise.
        }
    }

    private func attachToInflightMeeting() {
        // Re-sync to the live session: jump straight to Recording, hook
        // polling, render Pause UI from `dimmy_meeting_is_paused`.
        phase = .recording
        statusLabel = isPaused ? "Paused" : "Recording"
        subStatusLabel = "Reattached to in-flight meeting"
        isPaused = DimmyCore.shared.meetingIsPaused
        // Best-effort: pick the freshest meeting dir off disk so the
        // timer + transcript reflect the live session. The Rust side
        // also writes mtime as it appends transcripts.txt, so the
        // newest dir is the right one.
        if let dir = freshestMeetingDir() {
            activeMeetingDir = dir.path
            startedAt = (try? dir.resourceValues(forKeys: [.creationDateKey]).creationDate)
                ?? Date()
        } else {
            startedAt = Date()
        }
        startRecordingPolling()
    }

    // MARK: - Start

    func start() {
        guard !isWorking, phase == .idle || phase == .done else { return }
        isWorking = true
        phase = .recording
        statusLabel = "Starting…"
        subStatusLabel = ""
        chunkSummary = ""
        transcript = ""
        doneSections = [:]
        doneRawTranscript = ""
        doneAudioURL = nil
        browsingPastMeeting = false
        isPaused = false
        // Reset the event-driven live-transcript mirror (filled by
        // DimmyCore.handleEvent on every `meeting_chunk` event).
        AppState.shared.meetingActiveDir = ""
        AppState.shared.meetingLiveTranscript = ""
        AppState.shared.meetingChunkCount = 0
        AppState.shared.meetingLastChunkSpeaker = ""

        DispatchQueue.global(qos: .userInitiated).async {
            let id = DimmyCore.shared.meetingStart()
            DispatchQueue.main.async {
                self.isWorking = false
                guard let id, !id.isEmpty else {
                    self.phase = .idle
                    self.statusLabel = "Couldn't start"
                    self.subStatusLabel = "Check microphone permissions and try again."
                    return
                }
                self.sessionId = id
                self.startedAt = Date()
                self.statusLabel = "Recording"
                self.subStatusLabel = "Session id \(String(id.prefix(8)))…"
                self.titlebarTitle = "Recording…"
                self.startRecordingPolling()
                self.loadHistory()  // surfaces the new dir in the sidebar
                Task {
                    let ok = await SystemAudioCaptureService.shared.start()
                    if !ok {
                        self.showToast("System audio unavailable — mic only. Grant Screen Recording in System Settings → Privacy.")
                    }
                }
            }
        }
    }

    // MARK: - Pause / Resume

    /// Toggle the pause state on the in-flight meeting. Reads the live
    /// state from FFI before flipping so we don't get out of sync if
    /// another surface (pill, future tray menu) flipped it.
    func togglePause() {
        guard phase == .recording else {
            showToast("No active meeting to pause.")
            return
        }
        let currentlyPaused = DimmyCore.shared.meetingIsPaused
        if currentlyPaused {
            _ = DimmyCore.shared.meetingResume()
            isPaused = false
            statusLabel = "Recording"
            showToast("Resumed.")
        } else {
            _ = DimmyCore.shared.meetingPause()
            isPaused = true
            statusLabel = "Paused"
            showToast("Paused — audio + transcript skipped until you resume.")
        }
    }

    // MARK: - Stop

    func stopAndProcess() {
        guard phase == .recording, !isWorking else { return }
        isWorking = true
        phase = .processing
        processingStep = .saving
        statusLabel = "Stopping & finalising…"
        subStatusLabel = ""
        stopRecordingPolling()
        // Snapshot the live transcript into the Done buffer so the
        // chunks the user just saw stay visible while the recap
        // pipeline runs. AppState.meetingLiveTranscript will be
        // cleared on the next start().
        doneRawTranscript = AppState.shared.meetingLiveTranscript
        AppState.shared.meetingActiveDir = ""

        SystemAudioCaptureService.shared.stop()
        DispatchQueue.global(qos: .userInitiated).async {
            let result = DimmyCore.shared.meetingStop()
            DispatchQueue.main.async {
                self.isPaused = false
                guard let result else {
                    self.isWorking = false
                    self.phase = .idle
                    self.statusLabel = "Stop failed"
                    self.subStatusLabel = "See dimmy.log for details."
                    return
                }
                self.activeMeetingDir = result.dir
                let cleanTranscript = result.transcript.trimmingCharacters(in: .whitespacesAndNewlines)
                self.doneRawTranscript = cleanTranscript
                self.doneTitle = self.titleFromDir(result.dir)
                self.doneMeta = String(
                    format: "%.0fs · %d chunks", result.durationSecs, result.chunkCount
                )
                self.doneAudioURL = self.audioURL(for: result.dir)
                self.titlebarTitle = self.doneTitle

                if cleanTranscript.isEmpty {
                    // No speech captured (very short recording, VAD
                    // rejected everything, or STT failed silently).
                    // Don't pretend the recap "wasn't generated" — be
                    // explicit so the user knows nothing landed.
                    self.isWorking = false
                    self.phase = .done
                    self.doneSections = [
                        "TLDR": "Nothing was recorded — no speech was detected. Try moving closer to the microphone, checking input device settings, or recording for longer.",
                    ]
                    self.statusLabel = "Empty recording"
                    self.subStatusLabel = "No transcript was produced"
                    self.loadHistory()
                } else if self.generateRecap {
                    self.processingStep = .generatingRecap
                    self.runPostProcess(dir: result.dir, transcript: cleanTranscript)
                } else {
                    self.isWorking = false
                    self.phase = .done
                    self.doneSections = [
                        "TLDR": "Recap skipped — toggle on \"Generate recap\" before stopping next time, or click Regenerate to run it now.",
                    ]
                    self.statusLabel = "Done"
                    self.loadHistory()
                }
            }
        }
    }

    private func runPostProcess(dir: String, transcript: String) {
        statusLabel = "Generating recap with LLM…"
        let notionAutoSend = AppState.shared.notionAutoSend
        DispatchQueue.global(qos: .userInitiated).async {
            let result = MeetingPostProcessService.runRecap(
                dir: dir,
                transcript: transcript,
                notionAutoSend: notionAutoSend
            )
            DispatchQueue.main.async {
                self.isWorking = false
                self.phase = .done
                switch result {
                case .success(let recap):
                    self.processingStep = .extractingActions
                    self.doneSections = recap.sections
                    self.statusLabel = "Done"
                    self.subStatusLabel = "Recap saved to \(URL(fileURLWithPath: dir).lastPathComponent)"
                case .failure(let err):
                    self.doneSections = ["TLDR": "(recap failed — \(err))"]
                    self.statusLabel = "Recap failed"
                    self.subStatusLabel = "\(err)"
                }
                self.loadHistory()
            }
        }
    }

    // MARK: - Regenerate (Done state)

    /// Re-run the LLM recap from the persisted transcripts.txt. Useful
    /// when the prompt was tweaked or the recap got truncated.
    func regenerateRecap() {
        guard !activeMeetingDir.isEmpty || selectedDir != nil else { return }
        let dir = selectedDir ?? activeMeetingDir
        let transcriptURL = URL(fileURLWithPath: dir).appendingPathComponent("transcripts.txt")
        guard let transcript = try? String(contentsOf: transcriptURL, encoding: .utf8),
              !transcript.trimmingCharacters(in: .whitespaces).isEmpty
        else {
            showToast("No transcript on disk to recap.")
            return
        }
        phase = .processing
        processingStep = .generatingRecap
        statusLabel = "Regenerating recap…"
        let notionAutoSend = AppState.shared.notionAutoSend
        DispatchQueue.global(qos: .userInitiated).async {
            let result = MeetingPostProcessService.runRecap(
                dir: dir,
                transcript: transcript,
                notionAutoSend: notionAutoSend
            )
            DispatchQueue.main.async {
                self.phase = .done
                switch result {
                case .success(let recap):
                    self.doneSections = recap.sections
                    self.doneRawTranscript = transcript
                    self.statusLabel = "Recap regenerated"
                case .failure(let err):
                    self.statusLabel = "Recap failed"
                    self.subStatusLabel = "\(err)"
                }
            }
        }
    }

    /// Re-run the file-load transcribe path on `audio.wav` — replaces
    /// transcripts.txt + re-runs the recap. Useful when the live STT
    /// truncated or the user wants a different language pass.
    func regenerateTranscript() {
        guard !activeMeetingDir.isEmpty || selectedDir != nil else { return }
        let dir = selectedDir ?? activeMeetingDir
        let audioURL = URL(fileURLWithPath: dir).appendingPathComponent("audio.wav")
        guard FileManager.default.fileExists(atPath: audioURL.path) else {
            showToast("No audio.wav on disk to re-transcribe.")
            return
        }
        phase = .processing
        processingStep = .saving
        statusLabel = "Re-transcribing audio…"
        DispatchQueue.global(qos: .userInitiated).async {
            let result = DimmyCore.shared.transcribeFile(at: audioURL.path)
            DispatchQueue.main.async {
                switch result {
                case .success(let text):
                    let transcriptURL = URL(fileURLWithPath: dir).appendingPathComponent("transcripts.txt")
                    try? text.write(to: transcriptURL, atomically: true, encoding: .utf8)
                    self.doneRawTranscript = text
                    self.regenerateRecap()
                case .failure(let err):
                    self.phase = .done
                    self.statusLabel = "Re-transcribe failed"
                    self.subStatusLabel = "\(err)"
                }
            }
        }
    }

    // MARK: - Sidebar

    func loadHistory() {
        guard let baseDir = meetingsDir() else {
            historyRows = []
            return
        }
        let fm = FileManager.default
        guard let contents = try? fm.contentsOfDirectory(at: baseDir,
                                                          includingPropertiesForKeys: [.contentModificationDateKey, .creationDateKey],
                                                          options: [.skipsHiddenFiles])
        else {
            historyRows = []
            return
        }
        let dirs = contents.filter {
            (try? $0.resourceValues(forKeys: [.isDirectoryKey]).isDirectory) == true
        }
        let rows: [MeetingHistoryRow] = dirs.compactMap { url in
            let name = url.lastPathComponent
            let mtime = (try? url.resourceValues(forKeys: [.contentModificationDateKey]).contentModificationDate) ?? .distantPast
            let recapPath = url.appendingPathComponent("recap.md").path
            let hasRecap = fm.fileExists(atPath: recapPath)
            let title = MeetingHistoryRow.titleFor(dirName: name, recapPath: hasRecap ? recapPath : nil)
            let subtitle = MeetingHistoryRow.subtitleFor(date: mtime)
            return MeetingHistoryRow(
                dir: url.path,
                title: title,
                subtitle: subtitle,
                rightLabel: hasRecap ? "✓" : "",
                modifiedAt: mtime
            )
        }.sorted { $0.modifiedAt > $1.modifiedAt }
        let q = historySearch.lowercased()
        if q.isEmpty {
            historyRows = rows
        } else {
            historyRows = rows.filter {
                $0.title.lowercased().contains(q) || $0.subtitle.lowercased().contains(q)
            }
        }
    }

    func selectHistory(_ row: MeetingHistoryRow) {
        // While recording is in flight, viewing a past meeting must
        // not stop the recording — the user can browse, then click
        // "Back to live" to return.
        if phase == .recording {
            browsingPastMeeting = true
        }
        selectedDir = row.dir
        loadDoneFromDisk(dir: row.dir)
    }

    func backToLive() {
        guard DimmyCore.shared.meetingIsActive else { return }
        browsingPastMeeting = false
        selectedDir = nil
        attachToInflightMeeting()
    }

    func newMeeting() {
        if phase == .recording || phase == .processing {
            showToast("Stop the current recording first to start a new one.")
            return
        }
        selectedDir = nil
        browsingPastMeeting = false
        phase = .idle
        statusLabel = "Ready"
        subStatusLabel = "Click Start to begin a meeting recording"
        titlebarTitle = "New meeting"
        doneSections = [:]
        doneRawTranscript = ""
        doneAudioURL = nil
        transcript = ""
    }

    func deleteHistory(_ row: MeetingHistoryRow) {
        let dir = row.dir
        if dir == activeMeetingDir, phase == .recording || phase == .processing {
            showToast("Stop the current recording before deleting it.")
            return
        }
        let alert = NSAlert()
        alert.messageText = "Delete this meeting?"
        alert.informativeText = """
        This will permanently remove:

        \(URL(fileURLWithPath: dir).lastPathComponent)

        Includes audio (audio.wav, per-track WAVs), transcripts.txt, and recap.md.
        """
        alert.alertStyle = .warning
        alert.addButton(withTitle: "Delete")
        alert.addButton(withTitle: "Cancel")
        alert.buttons.first?.hasDestructiveAction = true
        let response = alert.runModal()
        guard response == .alertFirstButtonReturn else { return }

        if selectedDir == dir {
            selectedDir = nil
            doneSections = [:]
            doneRawTranscript = ""
            doneAudioURL = nil
            phase = DimmyCore.shared.meetingIsActive ? .recording : .idle
        }
        try? FileManager.default.removeItem(at: URL(fileURLWithPath: dir))
        loadHistory()
    }

    // MARK: - Live polling

    private func startRecordingPolling() {
        stopRecordingPolling()
        pollTimer = Timer.scheduledTimer(withTimeInterval: 2.0, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.pollTick() }
        }
        amplitudeTimer = Timer.scheduledTimer(withTimeInterval: 1.0 / 12.0, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.amplitudeTick() }
        }
        pollTick()
    }

    private func stopRecordingPolling() {
        pollTimer?.invalidate(); pollTimer = nil
        amplitudeTimer?.invalidate(); amplitudeTimer = nil
        liveAmplitudeBars = Array(
            repeating: .zero,
            count: liveAmplitudeBars.isEmpty
                ? Self.liveAmplitudeBarCount
                : liveAmplitudeBars.count
        )
    }

    private func pollTick() {
        if let started = startedAt {
            let secs = Int(Date().timeIntervalSince(started))
            let h = secs / 3600
            let m = (secs % 3600) / 60
            let s = secs % 60
            timerLabel = String(format: "%02d:%02d:%02d", h, m, s)
        }
        // Pause state now flows from the `meeting_state` event in
        // DimmyCore.handleEvent into AppState.shared.meetingIsPaused —
        // no need to re-poll FFI here. Mirror the shared flag so
        // existing `vm.isPaused` consumers in MeetingRecordingView
        // keep working without a constructor-signature change.
        let livePaused = AppState.shared.meetingIsPaused
        if livePaused != isPaused {
            isPaused = livePaused
            statusLabel = isPaused ? "Paused" : "Recording"
        }

        // Don't fight a sidebar selection: only refresh transcript if
        // the user is on the live view, not browsing a past meeting.
        if browsingPastMeeting { return }
        guard let url = activeMeetingURL() else { return }
        let transcriptsPath = url.appendingPathComponent("transcripts.txt")
        if let content = try? String(contentsOf: transcriptsPath, encoding: .utf8),
           !content.isEmpty {
            transcript = content
            let chunkCount = content.split(separator: "\n", omittingEmptySubsequences: true).count
            chunkSummary = "\(chunkCount) chunk\(chunkCount == 1 ? "" : "s")"
        }
    }

    private func amplitudeTick() {
        // Real amplitude: the meeting worker (`core/src/meeting.rs`)
        // pushes mic + system samples into the same `audio_buffer` /
        // `audio_buffer_secondary` that `dimmy_get_amplitude` and
        // `dimmy_get_loopback_amplitude` read, so the FFI surfaces the
        // genuine peak even though meeting mode bypasses the dictation
        // capture path. Display-AGC mirrors Win
        // `MeetingWindow.OnAmpTick`: `min(1, sqrt(raw) * 1.4)`.
        let micRaw = DimmyCore.shared.getAmplitude()
        let sysRaw = DimmyCore.shared.getLoopbackAmplitude()
        let mic = MeetingAmplitudeAGC.displayLevel(micRaw)
        let sys = MeetingAmplitudeAGC.displayLevel(sysRaw)
        liveAmplitudeBars = MeetingAmplitudeAGC.push(
            liveAmplitudeBars,
            mic: mic,
            system: sys,
            capacity: Self.liveAmplitudeBarCount
        )
    }

    // MARK: - Disk helpers

    private func meetingsDir() -> URL? {
        guard let support = try? FileManager.default.url(
            for: .applicationSupportDirectory,
            in: .userDomainMask,
            appropriateFor: nil,
            create: false
        ) else { return nil }
        return support.appendingPathComponent("dimmy/meetings", isDirectory: true)
    }

    private func freshestMeetingDir() -> URL? {
        guard let baseDir = meetingsDir() else { return nil }
        guard let contents = try? FileManager.default.contentsOfDirectory(
            at: baseDir,
            includingPropertiesForKeys: [.contentModificationDateKey],
            options: [.skipsHiddenFiles]
        ) else { return nil }
        return contents
            .filter { (try? $0.resourceValues(forKeys: [.isDirectoryKey]).isDirectory) == true }
            .compactMap { url -> (URL, Date)? in
                let date = (try? url.resourceValues(forKeys: [.contentModificationDateKey]).contentModificationDate) ?? .distantPast
                return (url, date)
            }
            .sorted { $0.1 > $1.1 }
            .first?.0
    }

    private func activeMeetingURL() -> URL? {
        if !activeMeetingDir.isEmpty {
            return URL(fileURLWithPath: activeMeetingDir)
        }
        return freshestMeetingDir()
    }

    private func audioURL(for dir: String) -> URL? {
        let url = URL(fileURLWithPath: dir).appendingPathComponent("audio.wav")
        return FileManager.default.fileExists(atPath: url.path) ? url : nil
    }

    private func titleFromDir(_ dir: String) -> String {
        // Convention: dir is named `2026-05-09T14-32-08` or similar;
        // turn the leading date into a friendly label. If the user
        // ever renames it we just fall back to the raw name.
        let name = URL(fileURLWithPath: dir).lastPathComponent
        return MeetingHistoryRow.titleFor(dirName: name, recapPath: nil)
    }

    func loadDoneFromDisk(dir: String) {
        let url = URL(fileURLWithPath: dir)
        doneTitle = titleFromDir(dir)
        let mtime = (try? url.resourceValues(forKeys: [.contentModificationDateKey]).contentModificationDate)
            ?? Date()
        doneMeta = MeetingHistoryRow.subtitleFor(date: mtime)

        let recapURL = url.appendingPathComponent("recap.md")
        if let recapMd = try? String(contentsOf: recapURL, encoding: .utf8), !recapMd.isEmpty {
            doneSections = MeetingPostProcessService.parseMarkdownIntoSections(recapMd)
        } else {
            doneSections = [:]
        }
        let transcriptURL = url.appendingPathComponent("transcripts.txt")
        doneRawTranscript = (try? String(contentsOf: transcriptURL, encoding: .utf8)) ?? ""
        doneAudioURL = audioURL(for: dir)
        if !browsingPastMeeting {
            phase = .done
            titlebarTitle = doneTitle
        }
    }

    // MARK: - Toast

    private func showToast(_ text: String) {
        toastDismissTask?.cancel()
        toastMessage = text
        toastDismissTask = Task { @MainActor in
            try? await Task.sleep(nanoseconds: 2_500_000_000)
            if !Task.isCancelled { self.toastMessage = nil }
        }
    }
}

// MARK: - History row

struct MeetingHistoryRow: Identifiable, Equatable {
    let id: String
    let dir: String
    let title: String
    let subtitle: String
    let rightLabel: String
    let modifiedAt: Date

    init(dir: String, title: String, subtitle: String, rightLabel: String, modifiedAt: Date) {
        self.id = dir
        self.dir = dir
        self.title = title
        self.subtitle = subtitle
        self.rightLabel = rightLabel
        self.modifiedAt = modifiedAt
    }

    /// Build a friendly title from the dir name + optional recap.md.
    /// Tier of fallbacks (matches the Win sidebar shape):
    ///   1. First meaningful heading from recap.md (skips structural
    ///      titles like "TL;DR" / "Context" + ===KEY=== markers).
    ///   2. Prettified `2026-05-09T14-32-08` shape if dir name looks
    ///      like a timestamp.
    ///   3. `"Meeting <first-8-chars>"` for UUID-shaped dir names —
    ///      mirrors `MeetingWindow.LoadHistory` on Win so the user
    ///      always gets a stable short label even when the recap
    ///      didn't generate.
    static func titleFor(dirName: String, recapPath: String?) -> String {
        if let path = recapPath,
           let body = try? String(contentsOfFile: path, encoding: .utf8) {
            for line in body.split(separator: "\n", omittingEmptySubsequences: true) {
                let trimmed = line.trimmingCharacters(in: .whitespaces)
                guard trimmed.hasPrefix("## ") || trimmed.hasPrefix("# ") else { continue }
                let label = trimmed
                    .replacingOccurrences(of: "##", with: "")
                    .replacingOccurrences(of: "#", with: "")
                    .trimmingCharacters(in: .whitespaces)
                if !label.isEmpty,
                   !label.contains("==="),
                   !structuralHeadingTitles.contains(label) {
                    return String(label.prefix(60))
                }
            }
        }
        let prettified = prettifyDirName(dirName)
        if prettified != dirName { return prettified }
        // UUID-shaped: take first 8 chars after stripping any trailing
        // `_suffix` (matches Win MeetingWindow.LoadHistory).
        let primary = dirName.components(separatedBy: "_").first ?? dirName
        if isUuidLike(primary) {
            return "Meeting \(String(primary.prefix(8)))"
        }
        return dirName
    }

    private static func isUuidLike(_ s: String) -> Bool {
        // UUID shape: 8-4-4-4-12 hex digits with dashes.
        guard s.count == 36 else { return false }
        let parts = s.split(separator: "-")
        guard parts.count == 5 else { return false }
        let lengths = parts.map(\.count)
        guard lengths == [8, 4, 4, 4, 12] else { return false }
        return parts.allSatisfy { part in
            part.allSatisfy { c in c.isHexDigit }
        }
    }

    /// Structural section titles emitted by `buildMarkdownFromSections`.
    /// Skipped by `titleFor` so the sidebar label reflects the actual
    /// meeting topic, not a recap section heading.
    private static let structuralHeadingTitles: Set<String> = [
        "Context", "TL;DR", "Highlights", "Narrative", "Key decisions",
        "Topics", "Actions", "Open questions", "Risks", "Next steps", "Follow-ups",
    ]

    static func subtitleFor(date: Date) -> String {
        let formatter = DateFormatter()
        formatter.dateStyle = .medium
        formatter.timeStyle = .short
        return formatter.string(from: date)
    }

    private static func prettifyDirName(_ name: String) -> String {
        // Drop a trailing `_uuid` if present, then convert
        // `2026-05-09T14-32-08` → `2026-05-09 14:32`. Best-effort —
        // we keep the raw name on parse failure.
        let primary = name.components(separatedBy: "_").first ?? name
        let parts = primary.components(separatedBy: "T")
        guard parts.count == 2 else { return name }
        let date = parts[0]
        let timeRaw = parts[1]
        let timeParts = timeRaw.split(separator: "-")
        guard timeParts.count >= 2 else { return name }
        return "\(date) \(timeParts[0]):\(timeParts[1])"
    }
}

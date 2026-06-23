import Foundation
import SwiftUI
import AppKit
import AVFoundation
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
// VM is a thin mirror over the FFI, it does NOT cache "is recording"
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
    @Published var doneSections: [String: String] = [:] {
        didSet {
            // Reflect the resolved meeting type into the override picker so the
            // user sees what the recap currently is and can re-pick + regen.
            let rt = doneSections["__TYPE__"] ?? ""
            let normalized = rt.isEmpty ? "auto" : rt
            if selectedMeetingType != normalized { selectedMeetingType = normalized }
        }
    }

    /// True when the last recap attempt (post-process or regenerate)
    /// failed. Drives the "Generate recap with Claude Desktop" recovery
    /// CTA in the Done view, so a silent recap failure becomes one click
    /// to the path that works (mirror of Win RefreshClaudeFallbackCta).
    @Published var recapFailed: Bool = false

    /// Meeting-type override for the recap. "auto" = let the model classify.
    /// Bound to the Done-view picker; passed into the next regenerateRecap().
    @Published var selectedMeetingType: String = "auto"
    @Published var doneRawTranscript: String = ""
    @Published var doneAudioURL: URL?
    /// Per-track mic WAV (`audio_mic.wav`). When both mic + system are
    /// present the Done audio card renders a mirrored-stereo waveform
    /// (mic UP from midline, system DOWN); when nil it falls back to
    /// the single-band waveform reading `audio.wav`.
    @Published var doneAudioMicURL: URL?
    /// Per-track system-loopback WAV (`audio_system.wav`). See
    /// `doneAudioMicURL` for the dual-band semantics.
    @Published var doneAudioSystemURL: URL?
    @Published var browsingPastMeeting: Bool = false  // true when the user clicked a sidebar row while recording is in flight
    @Published var processingStep: ProcessingStep = .saving

    // ── Done-view tab selection ────────────────────────────────────
    enum DoneTab: String, CaseIterable, Identifiable {
        case recap
        case transcript
        case notes
        var id: String { rawValue }
        var title: String {
            switch self {
            case .recap: return "Recap"
            case .transcript: return "Transcript"
            case .notes: return "Notes"
            }
        }
    }
    @Published var doneSelectedTab: DoneTab = .recap

    // ── Recording-view tab selection (Live transcript / Notes) ────
    /// Mirror of the Win Recording-view Notes tab. The Notes editor and
    /// the Done-view Notes editor share the SAME `doneNotes` buffer +
    /// `<dir>/notes.md` on disk, single store, the handover doc calls
    /// this out explicitly. So a note typed live survives into the Done
    /// view of the same meeting without an extra load step.
    enum RecordingTab: String, CaseIterable, Identifiable {
        case live
        case notes
        var id: String { rawValue }
        var title: String {
            switch self {
            case .live: return "Live transcript"
            case .notes: return "Notes"
            }
        }
    }
    @Published var recordingSelectedTab: RecordingTab = .live

    /// Local-only meeting notes, persisted as `<dir>/notes.md`. Loaded
    /// by `loadDoneFromDisk` and `loadNotes`; written by `saveNotes`.
    /// Mirror of the Win Notes tab (`SelectDoneTab(\"Notes\")`).
    @Published var doneNotes: String = ""

    // ── Toast (blocked-during-recording warning) ───────────────────
    @Published var toastMessage: String?

    /// Persistent (non-auto-dismissing) banner shown when the meeting is
    /// recording but the Core Audio tap never delivered a frame, i.e. the
    /// system-audio-recording TCC grant is missing. Unlike `toastMessage`
    /// this stays up with an "Open System Settings" CTA so the user can
    /// actually act on it (the 2.5 s toast vanished before they could).
    /// Cleared on stop / next start / once system audio starts flowing.
    @Published var systemAudioPermissionNeeded: Bool = false

    /// Determinate progress (0–100) for meeting re-transcription, mirrored
    /// from AppState.fileTranscribeProgress (the core emits
    /// `file_transcribe_progress` per chunk during dimmy_meeting_retranscribe).
    /// nil = not transcribing → the processing view shows its indeterminate
    /// spinner (recap / idle). Set to 0 by regenerateTranscript, cleared when
    /// the transcription pass returns.
    @Published var retranscribePercent: Double?

    /// Combine bag for the live-transcript pipe from AppState.
    /// DimmyCore.handleEvent writes every `meeting_chunk` event's
    /// `line` into `AppState.meetingLiveTranscript`; we mirror that
    /// onto `self.transcript` so the recording view binds to a
    /// single property on its own view-model (Combine-friendly,
    /// no AppState reach-through in the View).
    private var liveTranscriptBag = Set<AnyCancellable>()

    init() {
        // Replaces the absent 2 s file polling that Win used to run
        // on transcripts.txt, purely event-driven. Subscription is
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
        // Determinate progress for meeting re-transcription. The core emits
        // file_transcribe_progress per chunk (same event as file-load);
        // DimmyCore.handleEvent writes it to AppState.fileTranscribeProgress.
        // Only mirror while a re-transcription is in flight (retranscribePercent
        // set to 0 by regenerateTranscript) so a Settings file-load doesn't
        // move the meeting bar.
        AppState.shared.$fileTranscribeProgress
            .receive(on: DispatchQueue.main)
            .sink { [weak self] progress in
                guard let self, self.retranscribePercent != nil, let p = progress else { return }
                self.retranscribePercent = p.percent
            }
            .store(in: &liveTranscriptBag)
        // Pause state is also event-driven: DimmyCore.handleEvent
        // writes `meeting_state` → `AppState.meetingIsPaused`. Mirror
        // it onto `self.isPaused` (plus the statusLabel hint) so the
        // recording bar / banner read a single vm property instead of
        // reaching into AppState. Replaces the old 1 Hz mirror in
        // pollTick, no polling needed.
        AppState.shared.$meetingIsPaused
            .receive(on: DispatchQueue.main)
            .sink { [weak self] paused in
                guard let self else { return }
                if paused != self.isPaused {
                    self.isPaused = paused
                    self.statusLabel = paused ? "Paused" : "Recording"
                    // Freeze / resume the elapsed-time clock so the
                    // timer label matches the recording the worker
                    // actually keeps (meeting.rs gap-skips the paused
                    // window, the WAV + transcript exclude it). On
                    // pause: stamp the pause-start instant. On resume:
                    // fold the paused span into pausedAccumulator so
                    // pollTick subtracts it from the wall-clock delta.
                    if paused {
                        self.pauseStartedAt = Date()
                    } else if let pausedAt = self.pauseStartedAt {
                        self.pausedAccumulator += Date().timeIntervalSince(pausedAt)
                        self.pauseStartedAt = nil
                    }
                    self.pollTick()
                }
            }
            .store(in: &liveTranscriptBag)
        // Mirror Win: when an EXTERNAL surface (pill stop, call-detect
        // popup "Stop & recap", future tray menu...) stops the meeting,
        // Rust emits `meeting_state` with `active=false` which lands
        // in AppState.meetingActive via DimmyCore.handleEvent. Without
        // this subscription the meeting window stayed pinned in
        // `.recording` for the entire recap LLM duration (~10-30 s),
        // the user assumed nothing was happening and pressed Stop a
        // second time, racing the pill-side stop. Flip to `.processing`
        // here; `loadDoneFromDisk` (called by `meetingRecapSaved`)
        // lands us in `.done` once the recap is on disk.
        AppState.shared.$meetingActive
            .receive(on: DispatchQueue.main)
            .sink { [weak self] active in
                guard let self else { return }
                if !active, self.phase == .recording {
                    self.stopPollingForExternalStop()
                    self.phase = .processing
                    self.processingStep = .generatingRecap
                    self.statusLabel = "Wrapping up..."
                    self.subStatusLabel = ""
                    self.armWrapUpWatchdog()
                }
            }
            .store(in: &liveTranscriptBag)
    }

    /// Cancel the recording-mode timers when an external stop fires.
    /// Pulled out so the meeting_state subscription can reuse the same
    /// teardown the local Stop button does, without duplicating logic.
    private func stopPollingForExternalStop() {
        pollTimer?.invalidate()
        pollTimer = nil
        amplitudeTimer?.invalidate()
        amplitudeTimer = nil
    }

    /// Safety net for the external-stop → "Wrapping up..." transition. The
    /// happy path resolves via `meetingRecapSaved` → `loadDoneFromDisk`,
    /// but an empty meeting (no recap), a recap crash, or a dropped
    /// notification would otherwise pin the window in `.processing`
    /// forever, that's the "can't terminate the meeting" trap. If we're
    /// still processing after a generous window (a real recap is 10-60 s),
    /// force-resolve from disk so the user is never stranded.
    private func armWrapUpWatchdog() {
        wrapUpWatchdog?.invalidate()
        wrapUpWatchdog = Timer.scheduledTimer(withTimeInterval: 90, repeats: false) {
            [weak self] _ in
            Task { @MainActor in
                guard let self, self.phase == .processing else { return }
                let dir = self.activeMeetingDir.isEmpty
                    ? (self.freshestMeetingDir()?.path ?? "")
                    : self.activeMeetingDir
                if dir.isEmpty {
                    self.phase = .done
                    self.statusLabel = "Done"
                    self.subStatusLabel = "Meeting finished"
                } else {
                    self.loadDoneFromDisk(dir: dir)
                }
                self.isWorking = false
                self.loadHistory()
            }
        }
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
    /// Total time the meeting has spent paused, accumulated across
    /// every pause/resume cycle. Subtracted from the wall-clock delta
    /// in `pollTick` so the timer label tracks recorded duration.
    private var pausedAccumulator: TimeInterval = 0
    /// Instant the current pause began, or nil when not paused. While
    /// non-nil, `pollTick` freezes the clock at this instant.
    private var pauseStartedAt: Date?
    private var pollTimer: Timer?
    private var amplitudeTimer: Timer?
    /// One-shot safety net for the external-stop "Wrapping up..." state.
    /// If no recap lands us in `.done`, this force-resolves from disk so
    /// the window can never strand the user in processing limbo.
    private var wrapUpWatchdog: Timer?
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
            // Nothing to attach to, keep current state. If we just
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

        // Prefer the already-attached `activeMeetingDir` when we have
        // one, that's the cheap-and-correct path for "Back to live"
        // after the user browsed a past meeting in the sidebar. Falling
        // back to `freshestMeetingDir()` was risky: it sorts by mtime,
        // and any file we just wrote inside a past dir (transcript
        // load, notes save) would mask the real live dir.
        //
        // Likewise, leave `startedAt` alone when it's already set , 
        // the pollTimer clock has been ticking since `start()` and
        // re-deriving from the dir's `creationDate` introduces drift
        // (FS mtime granularity, copy-on-write timestamps).
        if activeMeetingDir.isEmpty, let dir = freshestMeetingDir() {
            activeMeetingDir = dir.path
        }
        if startedAt == nil {
            if !activeMeetingDir.isEmpty {
                let url = URL(fileURLWithPath: activeMeetingDir)
                startedAt = (try? url.resourceValues(forKeys: [.creationDateKey]).creationDate)
                    ?? Date()
            } else {
                startedAt = Date()
            }
        }
        startRecordingPolling()
    }

    // MARK: - Start

    func start() {
        guard !isWorking, phase == .idle || phase == .done else { return }
        // Recording-consent gate (mandatory). A meeting captures system audio
        // = other people, so we confirm consent and announce before recording.
        // Cancelling aborts the start. Mirror of Win MeetingWindow.Start_Click.
        let lang = Locale.current.language.languageCode?.identifier ?? "en"
        guard MeetingConsentFlow.confirmAndAnnounce(lang: lang) else { return }
        // Flush any unsaved notes from the previous Done view before
        // we wipe the buffer, matches the LostFocus save on Win.
        saveNotes()
        isWorking = true
        phase = .recording
        statusLabel = "Starting..."
        subStatusLabel = ""
        chunkSummary = ""
        transcript = ""
        doneSections = [:]
        doneRawTranscript = ""
        doneAudioURL = nil
        doneAudioMicURL = nil
        doneAudioSystemURL = nil
        doneNotes = ""
        doneSelectedTab = .recap
        // Recording view always opens on Live transcript, a previous
        // meeting that ended on the Notes tab shouldn't carry over.
        recordingSelectedTab = .live
        browsingPastMeeting = false
        systemAudioPermissionNeeded = false
        isPaused = false
        // Fresh pause-clock state for the new meeting.
        pausedAccumulator = 0
        pauseStartedAt = nil
        timerLabel = "00:00:00"
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
                // Pin the recap choice for THIS meeting, every stop path
                // (window, pill, call-detect popup) reads
                // AppState.meetingGenerateRecap so a stale checkbox on a
                // reopened window can't flip it. Mirror of Win
                // AppViewModel.MeetingGenerateRecap captured at start.
                AppState.shared.meetingGenerateRecap = self.generateRecap
                self.statusLabel = "Recording"
                self.subStatusLabel = "Session id \(String(id.prefix(8)))..."
                self.titlebarTitle = "Recording..."
                self.startRecordingPolling()
                self.loadHistory()  // surfaces the new dir in the sidebar
                Task {
                    let ok = await SystemAudioCaptureService.shared.start()
                    if !ok {
                        self.systemAudioPermissionNeeded = true
                        return
                    }
                    // The Core Audio tap is created even when the audio-
                    // recording grant is missing; it just never delivers
                    // frames. If none arrive shortly, raise a persistent
                    // banner (with an Open-Settings CTA) instead of silently
                    // recording mic-only. Guard on sessionId so a
                    // stopped/replaced meeting can't fire a stale banner.
                    try? await Task.sleep(nanoseconds: 1_800_000_000)
                    guard self.sessionId == id else { return }
                    if SystemAudioCaptureService.shared.isCapturingSystemAudio {
                        self.systemAudioPermissionNeeded = false
                    } else {
                        self.systemAudioPermissionNeeded = true
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
            showToast("Paused, audio + transcript skipped until you resume.")
        }
    }

    // MARK: - Stop

    func stopAndProcess() {
        guard phase == .recording, !isWorking else { return }
        isWorking = true
        phase = .processing
        processingStep = .saving
        statusLabel = "Stopping & finalising..."
        subStatusLabel = ""
        stopRecordingPolling()
        systemAudioPermissionNeeded = false
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
                self.doneAudioMicURL = self.micAudioURL(for: result.dir)
                self.doneAudioSystemURL = self.systemAudioURL(for: result.dir)
                self.doneNotes = Self.readNotes(dir: result.dir)
                self.doneSelectedTab = .recap
                self.titlebarTitle = self.doneTitle

                if cleanTranscript.isEmpty {
                    // No speech captured (very short recording, VAD
                    // rejected everything, or STT failed silently).
                    // Don't pretend the recap "wasn't generated", be
                    // explicit so the user knows nothing landed.
                    self.isWorking = false
                    self.phase = .done
                    self.doneSections = [
                        "TLDR": "Nothing was recorded, no speech was detected. Try moving closer to the microphone, checking input device settings, or recording for longer.",
                    ]
                    self.statusLabel = "Empty recording"
                    self.subStatusLabel = "No transcript was produced"
                    self.loadHistory()
                } else if AppState.shared.meetingGenerateRecap {
                    // Read the flag pinned at meeting start, NOT the live
                    // checkbox (which a reopened window can render stale).
                    self.processingStep = .generatingRecap
                    self.runPostProcess(dir: result.dir, transcript: cleanTranscript)
                } else {
                    self.isWorking = false
                    self.phase = .done
                    self.doneSections = [
                        "TLDR": "Recap skipped, toggle on \"Generate recap\" before stopping next time, or click Regenerate to run it now.",
                    ]
                    self.statusLabel = "Done"
                    self.loadHistory()
                }
            }
        }
    }

    private func runPostProcess(dir: String, transcript: String) {
        statusLabel = "Generating recap with LLM..."
        let notionAutoSend = AppState.shared.notionAutoSend
        let meetingType = selectedMeetingType == "auto" ? "" : selectedMeetingType
        DispatchQueue.global(qos: .userInitiated).async {
            let result = MeetingPostProcessService.runRecap(
                dir: dir,
                transcript: transcript,
                notionAutoSend: notionAutoSend,
                meetingType: meetingType
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
                    self.recapFailed = false
                case .failure(let err):
                    self.doneSections = ["TLDR": "(recap failed, \(err))"]
                    self.statusLabel = "Recap failed"
                    self.subStatusLabel = "\(err)"
                    self.recapFailed = true
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
        statusLabel = "Regenerating recap..."
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
                    self.recapFailed = false
                case .failure(let err):
                    self.statusLabel = "Recap failed"
                    self.subStatusLabel = "\(err)"
                    self.recapFailed = true
                }
            }
        }
    }

    /// Rebuild `transcripts.txt` from the per-band audio
    /// (`audio_mic.*` / `audio_system.*`, or `audio.*` as mic fallback)
    /// in the live-worker format `[<ms> ms] [band] text`, interleaved by
    /// time — same shape the meeting worker produces live. The Rust side
    /// (`dimmy_meeting_retranscribe`) decodes each band, runs the active
    /// STT backend (local or cloud, backend-aware chunking), writes
    /// `transcripts.txt` itself, and returns the merged text. Then we
    /// re-run the recap. Useful when the live STT truncated or the user
    /// wants a fresh pass.
    func regenerateTranscript() {
        guard !activeMeetingDir.isEmpty || selectedDir != nil else { return }
        let dir = selectedDir ?? activeMeetingDir
        phase = .processing
        processingStep = .saving
        statusLabel = "Re-transcribing audio..."
        // Arm the determinate bar — the file_transcribe_progress sink only
        // mirrors while this is non-nil.
        retranscribePercent = 0
        DispatchQueue.global(qos: .userInitiated).async {
            let result = DimmyCore.shared.meetingRetranscribe(dir: dir)
            DispatchQueue.main.async {
                // Transcription pass done — clear the determinate bar so the
                // recap step (no progress events) falls back to the spinner.
                self.retranscribePercent = nil
                switch result {
                case .success(let text):
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
            // Date comes from meta.json `started_at`, NOT the dir mtime , 
            // editing a title rewrites meta.json + bumps the dir mtime, so
            // an mtime sort reordered the meeting and showed "now" as its
            // date. See MeetingHistoryRow.dateFor.
            let date = MeetingHistoryRow.dateFor(dirURL: url)
            let recapPath = url.appendingPathComponent("recap.md").path
            let hasRecap = fm.fileExists(atPath: recapPath)
            let metaPath = url.appendingPathComponent("meta.json").path
            let title = MeetingHistoryRow.titleFor(
                dirName: name,
                recapPath: hasRecap ? recapPath : nil,
                metaPath: fm.fileExists(atPath: metaPath) ? metaPath : nil
            )
            let subtitle = MeetingHistoryRow.subtitleFor(date: date)
            return MeetingHistoryRow(
                dir: url.path,
                title: title,
                subtitle: subtitle,
                rightLabel: hasRecap ? "✓" : "",
                modifiedAt: date
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
        // Flush pending notes BEFORE we switch selectedDir. saveNotes
        // resolves the target via `notesTargetDir` which prefers
        // `selectedDir`, so doing it after the assignment would write
        // the LIVE meeting's (typically empty) notes buffer into the
        // PAST meeting's `notes.md`, either overwriting or deleting it.
        // The empty-notes branch in saveNotes also touches the past
        // dir's mtime, which then makes `freshestMeetingDir()` return
        // the past dir on the next "Back to live", so the timer label
        // jumps to the past meeting's age. This single re-ordering
        // fixes both bugs.
        saveNotes()
        if phase == .recording {
            browsingPastMeeting = true
        }
        selectedDir = row.dir
        loadDoneFromDisk(dir: row.dir)
    }

    func backToLive() {
        guard DimmyCore.shared.meetingIsActive else { return }
        // Flush any pending notes for the past meeting BEFORE clearing
        // selectedDir. MeetingDoneView fires `vm.saveNotes()` on its
        // `.onDisappear`, but by that point selectedDir would already
        // be nil → `notesTargetDir` falls back to `activeMeetingDir`
        // → the past meeting's `doneNotes` buffer would be written
        // into the LIVE meeting's `notes.md`. Saving here (while
        // selectedDir still points at the past meeting) lets the
        // onDisappear save become a harmless idempotent rewrite.
        saveNotes()
        browsingPastMeeting = false
        selectedDir = nil
        attachToInflightMeeting()
    }

    func newMeeting() {
        if phase == .recording || phase == .processing {
            showToast("Stop the current recording first to start a new one.")
            return
        }
        saveNotes()
        selectedDir = nil
        browsingPastMeeting = false
        phase = .idle
        statusLabel = "Ready"
        subStatusLabel = "Click Start to begin a meeting recording"
        titlebarTitle = "New meeting"
        doneSections = [:]
        doneRawTranscript = ""
        doneAudioURL = nil
        doneAudioMicURL = nil
        doneAudioSystemURL = nil
        doneNotes = ""
        doneSelectedTab = .recap
        // Recording view always opens on Live transcript, a previous
        // meeting that ended on the Notes tab shouldn't carry over.
        recordingSelectedTab = .live
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
        // 1 Hz, CLAUDE.md "documented exceptions" lists the recording
        // clock at 1 Hz for the elapsed-time label. The old 2 s
        // interval made the timer visibly jump (00:00 → 00:02 → 00:04)
        // and felt frozen. pollTick is cheap (Date diff + a Combine
        // mirror); ticking at 1 Hz is well within budget. FFI poll for
        // pause state was already removed (event-driven via
        // `meeting_state`), so no extra Rust work.
        pollTimer = Timer.scheduledTimer(withTimeInterval: 1.0, repeats: true) { [weak self] _ in
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
        // Pure local clock, no FFI poll, no disk read. CLAUDE.md
        // "documented exceptions" allows the recording clock at 1 Hz.
        // Pause state and live transcript come in via Combine from
        // AppState (event-driven, hooked in init()), so they're NOT
        // mirrored here anymore. Before this cleanup, the function:
        //   • re-read AppState.meetingIsPaused on every tick
        //   • re-read transcripts.txt off disk on every tick AND
        //     overwrote the Combine-driven `self.transcript`,
        //     creating a redundant race against the `meeting_chunk`
        //     event pipe.
        // Both removed, the timer label is the only thing left.
        guard let started = startedAt else { return }
        // While paused, freeze the clock at the pause-start instant;
        // otherwise use now. Subtract every paused span so the label
        // reflects recorded duration, not wall-clock since start.
        let mark = pauseStartedAt ?? Date()
        let elapsed = mark.timeIntervalSince(started) - pausedAccumulator
        let secs = Int(max(0, elapsed))
        let h = secs / 3600
        let m = (secs % 3600) / 60
        let s = secs % 60
        timerLabel = String(format: "%02d:%02d:%02d", h, m, s)
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
        // Effective meetings dir from the Rust core, honours the user's
        // `meeting_storage_path` override AND the flavor-aware default
        // (`dimmy`/`dimmy-staging`). Resolved fresh each call so a
        // runtime change of the storage dir is picked up. Never re-derive
        // `configDirURL/meetings` here, that bypasses the override.
        DimmyCore.shared.meetingsDirURL
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
        return Self.resolveMeetingAudio(dir: dir, base: "audio")
    }

    private func micAudioURL(for dir: String) -> URL? {
        return Self.resolveMeetingAudio(dir: dir, base: "audio_mic")
    }

    private func systemAudioURL(for dir: String) -> URL? {
        return Self.resolveMeetingAudio(dir: dir, base: "audio_system")
    }

    /// Resolve a meeting audio track to its on-disk URL, preferring the
    /// newer Ogg/Vorbis file (`feat/meeting-live-notes`) over the older
    /// WAV. Returns nil iff neither exists.
    ///
    /// Why .ogg first: once the `meeting.rs::TrackSink::create` gate is
    /// widened to Mac, fresh meetings only emit `.ogg`. Older meetings
    /// (pre-gate) stay `.wav`, the fallback keeps them playable +
    /// re-transcribable. A single resolver is used by every path that
    /// hardcoded `audio*.wav` (playback URLs, regenerate-transcript, the
    /// mtime sort), so adding new audio surfaces touches one method, not
    /// six. Pure / nonisolated so `MeetingAudioResolverTests` can pin
    /// the precedence on real tmp files without spinning up a ViewModel.
    nonisolated static func resolveMeetingAudio(dir: String, base: String) -> URL? {
        let oggURL = URL(fileURLWithPath: dir).appendingPathComponent(base + ".ogg")
        if FileManager.default.fileExists(atPath: oggURL.path) { return oggURL }
        let wavURL = URL(fileURLWithPath: dir).appendingPathComponent(base + ".wav")
        if FileManager.default.fileExists(atPath: wavURL.path) { return wavURL }
        return nil
    }

    private func titleFromDir(_ dir: String) -> String {
        // Convention: dir is named `2026-05-09T14-32-08` or similar;
        // turn the leading date into a friendly label. If the user
        // ever renames it we just fall back to the raw name.
        let url = URL(fileURLWithPath: dir)
        let name = url.lastPathComponent
        let metaPath = url.appendingPathComponent("meta.json").path
        let recapPath = url.appendingPathComponent("recap.md").path
        let fm = FileManager.default
        return MeetingHistoryRow.titleFor(
            dirName: name,
            recapPath: fm.fileExists(atPath: recapPath) ? recapPath : nil,
            metaPath: fm.fileExists(atPath: metaPath) ? metaPath : nil
        )
    }

    /// Persist a user-edited meeting title to `meta.json`. Mac mirror
    /// of Win's `WriteMetaTitle` in MeetingWindow.xaml.cs. Trims,
    /// rejects empty / >200-char titles. After the write the caller
    /// should refresh the active row + the sidebar so the new label
    /// shows up everywhere.
    func renameSelectedMeeting(to newTitle: String) {
        let activeDir = activeMeetingDir.isEmpty ? nil : activeMeetingDir
        guard let dir = selectedDir ?? activeDir, !dir.isEmpty else { return }
        let trimmed = newTitle.trimmingCharacters(in: .whitespaces)
        guard !trimmed.isEmpty, trimmed.count <= 200, trimmed != doneTitle else { return }
        let metaPath = (dir as NSString).appendingPathComponent("meta.json")
        var obj: [String: Any] = [:]
        if let data = try? Data(contentsOf: URL(fileURLWithPath: metaPath)),
           let existing = try? JSONSerialization.jsonObject(with: data) as? [String: Any] {
            obj = existing
        }
        obj["title"] = trimmed
        if let data = try? JSONSerialization.data(withJSONObject: obj, options: [.prettyPrinted, .sortedKeys]) {
            try? data.write(to: URL(fileURLWithPath: metaPath), options: .atomic)
        }
        doneTitle = trimmed
        titlebarTitle = trimmed
        if let idx = historyRows.firstIndex(where: { $0.dir == dir }) {
            let old = historyRows[idx]
            historyRows[idx] = MeetingHistoryRow(
                dir: old.dir,
                title: trimmed,
                subtitle: old.subtitle,
                rightLabel: old.rightLabel,
                modifiedAt: old.modifiedAt
            )
        }
    }

    func loadDoneFromDisk(dir: String) {
        // We reached a terminal state, cancel the wrap-up safety net.
        wrapUpWatchdog?.invalidate()
        wrapUpWatchdog = nil
        // Callers are responsible for flushing pending notes BEFORE
        // they call us (via `saveNotes()` while `selectedDir` still
        // points at the previous target). Doing the save here would
        // resolve `notesTargetDir` to the NEW dir we're loading and
        // happily blow away its `notes.md`. See `selectHistory` for
        // the canonical order.
        let url = URL(fileURLWithPath: dir)
        doneTitle = titleFromDir(dir)
        // Meeting date from meta.json `started_at` (stable across title
        // edits), not the dir mtime, see MeetingHistoryRow.dateFor.
        doneMeta = MeetingHistoryRow.subtitleFor(date: MeetingHistoryRow.dateFor(dirURL: url))

        let recapURL = url.appendingPathComponent("recap.md")
        if let recapMd = try? String(contentsOf: recapURL, encoding: .utf8), !recapMd.isEmpty {
            doneSections = MeetingPostProcessService.parseMarkdownIntoSections(recapMd)
        } else {
            doneSections = [:]
        }
        let transcriptURL = url.appendingPathComponent("transcripts.txt")
        doneRawTranscript = (try? String(contentsOf: transcriptURL, encoding: .utf8)) ?? ""
        doneAudioURL = audioURL(for: dir)
        doneAudioMicURL = micAudioURL(for: dir)
        doneAudioSystemURL = systemAudioURL(for: dir)
        doneNotes = Self.readNotes(dir: dir)
        doneSelectedTab = .recap
        if !browsingPastMeeting {
            phase = .done
            titlebarTitle = doneTitle
        }
    }

    // MARK: - Notes (local-only, persisted as <dir>/notes.md)

    /// Resolve the dir notes should land in: explicit sidebar selection
    /// wins, otherwise fall back to the just-finished meeting. Returns
    /// nil when there's no concrete meeting on screen yet.
    private var notesTargetDir: String? {
        if let sel = selectedDir, !sel.isEmpty { return sel }
        if !activeMeetingDir.isEmpty { return activeMeetingDir }
        return nil
    }

    /// Read `<dir>/notes.md` into a string, or "" if missing. Static so
    /// the stop / loadDoneFromDisk paths can hydrate `doneNotes` in one
    /// line without going through an instance method.
    private static func readNotes(dir: String) -> String {
        let url = URL(fileURLWithPath: dir).appendingPathComponent("notes.md")
        return (try? String(contentsOf: url, encoding: .utf8)) ?? ""
    }

    /// Append a `[mm:ss] ` time stamp at the end of the notes buffer so
    /// the user can type the note after it. Mirror of the Win Recording-
    /// view "Add note" / Ctrl+Enter behaviour. Uses the meeting elapsed
    /// time (the same monotonic clock the recording bar shows). No-op
    /// before a meeting has started, guards against accidental invokes
    /// from the Done view (which has its own meta time, not elapsed).
    func stampMeetingTime() {
        guard phase == .recording else { return }
        doneNotes = Self.stamping(notes: doneNotes, timerLabel: timerLabel)
    }

    /// Pure: the body of `stampMeetingTime()` factored out so the format
    /// (trim `HH:` prefix, insert newline if needed) is pinned by
    /// `MeetingStampTests` without spinning up a real ViewModel.
    /// `internal` access so the test target reaches it via @testable.
    nonisolated static func stamping(notes: String, timerLabel: String) -> String {
        let stamp: String
        if timerLabel.count >= 8 {
            // "HH:MM:SS" → strip "HH:" to match Win "[mm:ss]" shape.
            let idx = timerLabel.index(timerLabel.startIndex, offsetBy: 3)
            stamp = "[" + String(timerLabel[idx...]) + "] "
        } else {
            stamp = "[" + timerLabel + "] "
        }
        let separator: String
        if notes.isEmpty {
            separator = ""
        } else if notes.hasSuffix("\n") {
            separator = ""
        } else {
            separator = "\n"
        }
        return notes + separator + stamp
    }

    /// Write the current `doneNotes` buffer to `<dir>/notes.md`. No-op
    /// when there's no target dir (Idle state), avoids creating an
    /// orphan notes file on disk. Mirrors the Win LostFocus save.
    func saveNotes() {
        guard let dir = notesTargetDir else { return }
        let url = URL(fileURLWithPath: dir).appendingPathComponent("notes.md")
        // Empty notes → delete any leftover file so the meeting dir
        // stays clean. Best-effort: a write failure leaves the file
        // alone, which is fine.
        if doneNotes.isEmpty {
            try? FileManager.default.removeItem(at: url)
            return
        }
        try? doneNotes.write(to: url, atomically: true, encoding: .utf8)
    }

    // MARK: - System-audio permission banner

    /// Deep-link to System Settings → Privacy & Security so the user can
    /// grant Dimmy the system-audio-recording permission the Core Audio
    /// tap needs. We open the Privacy & Security root rather than a
    /// specific anchor because the audio-capture (process-tap) toggle's
    /// pane name is not stable across macOS 14/15; the banner copy tells
    /// the user what to look for.
    func openSystemAudioSettings() {
        guard let url = URL(
            string: "x-apple.systempreferences:com.apple.preference.security?Privacy"
        ) else { return }
        NSWorkspace.shared.open(url)
    }

    /// User-dismiss for the persistent permission banner.
    func dismissSystemAudioBanner() {
        systemAudioPermissionNeeded = false
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
    ///   3. `"Meeting <first-8-chars>"` for UUID-shaped dir names , 
    ///      mirrors `MeetingWindow.LoadHistory` on Win so the user
    ///      always gets a stable short label even when the recap
    ///      didn't generate.
    static func titleFor(dirName: String, recapPath: String?, metaPath: String? = nil) -> String {
        // Highest priority: meta.json["title"], written by the Rust
        // core's save_post_process (parse_recap_title) AND by the
        // Done view click-to-edit handler. Mirror of Win's metaTitle
        // lookup in MeetingWindow.xaml.cs LoadHistory.
        if let mp = metaPath,
           let data = try? Data(contentsOf: URL(fileURLWithPath: mp)),
           let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
           let t = obj["title"] as? String,
           !t.trimmingCharacters(in: .whitespaces).isEmpty {
            return String(t.prefix(120))
        }
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

    /// Canonical meeting timestamp for BOTH list ordering and the
    /// displayed date. Precedence: meta.json `started_at` (epoch secs,
    /// written at record start) → meta.json `ended_at` → `audio.wav`
    /// mtime → directory mtime → distantPast.
    ///
    /// Reading the epoch from meta.json, instead of the directory mtime , 
    /// is what keeps a meeting's date STABLE when the user edits its
    /// title: a rename rewrites meta.json and bumps the dir mtime, which
    /// (under the old mtime-sort) made the meeting jump to the top of the
    /// list and show "now" as its date. The Rust core preserves
    /// `started_at` across the stop-time finalize + title edits.
    static func dateFor(dirURL: URL) -> Date {
        let metaURL = dirURL.appendingPathComponent("meta.json")
        if let data = try? Data(contentsOf: metaURL),
           let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any] {
            if let started = obj["started_at"] as? Double, started > 0 {
                return Date(timeIntervalSince1970: started)
            }
            if let ended = obj["ended_at"] as? Double, ended > 0 {
                return Date(timeIntervalSince1970: ended)
            }
        }
        // Sort by mix-track mtime (audio.ogg on newer meetings, audio.wav
        // on older). resolveMeetingAudio returns nil iff neither exists.
        // Qualified to MeetingViewModel, this static lives on
        // MeetingHistoryRow so `Self` would resolve there.
        if let audioURL = MeetingViewModel.resolveMeetingAudio(
            dir: dirURL.path, base: "audio"),
           let m = try? audioURL.resourceValues(forKeys: [.contentModificationDateKey])
            .contentModificationDate {
            return m
        }
        if let m = try? dirURL.resourceValues(forKeys: [.contentModificationDateKey]).contentModificationDate {
            return m
        }
        return .distantPast
    }

    private static func prettifyDirName(_ name: String) -> String {
        // Drop a trailing `_uuid` if present, then convert
        // `2026-05-09T14-32-08` → `2026-05-09 14:32`. Best-effort , 
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

/// Mandatory recording-consent gate for meeting start. Mirror of the Windows
/// ConsentFlow: notice text + audit log come from the shared Rust core so every
/// platform says the same thing. The spoken notice reaches the user and anyone
/// in the same room; REMOTE participants only hear it if the user is unmuted,
/// so the pasteboard copy is the reliable channel for them. TTS / clipboard are
/// best-effort and never block the meeting.
@MainActor
enum MeetingConsentFlow {
    // Held statically so speech isn't cut off when the call returns.
    private static let synthesizer = AVSpeechSynthesizer()

    /// Shows the confirmation modal; on accept speaks + copies the announcement
    /// and logs each step. Returns true if the meeting may start. Main thread.
    static func confirmAndAnnounce(lang: String) -> Bool {
        let modal = DimmyCore.shared.consentText(kind: "modal", lang: lang)
            ?? "You are about to record audio that may include other people. Confirm you have informed all participants and obtained their consent."
        let announcement = DimmyCore.shared.consentText(kind: "announcement", lang: lang)
            ?? "Quick note: this meeting is being recorded and transcribed for note-taking."

        // Localized chrome from the shared core (parity with Windows).
        let title = DimmyCore.shared.consentText(kind: "title", lang: lang) ?? "Recording notice"
        let confirmLabel = DimmyCore.shared.consentText(kind: "confirm", lang: lang) ?? "I have consent, start"
        let cancelLabel = DimmyCore.shared.consentText(kind: "cancel", lang: lang) ?? "Cancel"

        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = title
        alert.informativeText = modal + "\n\n\u{201C}" + announcement + "\u{201D}"
        alert.addButton(withTitle: confirmLabel)
        alert.addButton(withTitle: cancelLabel)
        // Highlight Cancel (the second button) so Enter doesn't blow past the gate.
        if alert.buttons.count > 1 {
            alert.window.defaultButtonCell = alert.buttons[1].cell as? NSButtonCell
        }

        guard alert.runModal() == .alertFirstButtonReturn else {
            DimmyCore.shared.consentLogEvent(kind: "declined", lang: lang)
            return false
        }
        DimmyCore.shared.consentLogEvent(kind: "confirmed", lang: lang)

        // Chat message for participants (reliable channel for remotes).
        let pb = NSPasteboard.general
        pb.clearContents()
        pb.setString(announcement, forType: .string)
        DimmyCore.shared.consentLogEvent(kind: "chat_copied", lang: lang)

        // Speak it (reaches remotes only if the user is unmuted).
        let utterance = AVSpeechUtterance(string: announcement)
        if let voice = AVSpeechSynthesisVoice(language: lang) {
            utterance.voice = voice
        }
        synthesizer.speak(utterance)
        DimmyCore.shared.consentLogEvent(kind: "announced", lang: lang)
        return true
    }
}

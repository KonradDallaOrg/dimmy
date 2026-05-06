import AppKit
import SwiftUI
import Combine

// MARK: - MeetingViewModel
//
// Holds the meeting UI state. The Rust core owns the actual recording
// session (`MEETING` static), so this VM is mostly mirroring whatever
// is live on disk + driving the start / stop / post-process FFI calls.
//
// Live transcript is read from the meeting's `transcripts.txt` file
// every 2 s — same approach as Win MeetingWindow.OnPollTick. The file
// is monotonically appended by the Rust worker, so polling is cheap
// (O(file size) per tick, but the file stays small — one line per
// ~15 s chunk).

@MainActor
final class MeetingViewModel: ObservableObject {
    // ── Published state ────────────────────────────────────────────
    @Published var isRecording: Bool = false
    @Published var isWorking: Bool = false  // start / stop / post-process in flight
    @Published var generateRecap: Bool = true
    @Published var transcript: String = ""
    @Published var recap: String = ""
    @Published var actions: String = ""
    @Published var recapVisible: Bool = false
    @Published var actionsVisible: Bool = false
    @Published var activeDir: String = ""
    @Published var statusLabel: String = "Ready"
    @Published var subStatusLabel: String = "Click Start to begin a meeting recording"
    @Published var timerLabel: String = "00:00:00"
    @Published var chunkSummary: String?
    @Published var isPulsing: Bool = false  // drives the dot pulse animation

    // ── Internals ──────────────────────────────────────────────────
    private var startedAt: Date?
    private var pollTimer: Timer?

    /// The session id returned by `dimmy_meeting_start`. Held only for
    /// logging / debugging — we don't pass it to other FFI calls
    /// (Rust looks the active session up via the MEETING mutex).
    private var sessionId: String = ""

    var dotColor: Color {
        if !isRecording {
            return statusLabel.contains("Done") ? .green : .gray
        }
        return .red
    }

    // MARK: - Lifecycle

    func start() {
        guard !isRecording, !isWorking else { return }
        isWorking = true
        statusLabel = "Starting…"
        subStatusLabel = ""
        chunkSummary = nil
        transcript = ""
        recap = ""
        actions = ""
        recapVisible = false
        actionsVisible = false

        DispatchQueue.global(qos: .userInitiated).async {
            let id = DimmyCore.shared.meetingStart()
            DispatchQueue.main.async {
                self.isWorking = false
                guard let id, !id.isEmpty else {
                    self.statusLabel = "Couldn't start"
                    self.subStatusLabel = "Check microphone permissions and try again."
                    return
                }
                self.sessionId = id
                self.isRecording = true
                self.isPulsing = true
                self.startedAt = Date()
                self.statusLabel = "Recording"
                self.subStatusLabel = "Session id \(String(id.prefix(8)))…"
                self.startPolling()
            }
        }
    }

    func stopAndProcess() {
        guard isRecording, !isWorking else { return }
        isWorking = true
        statusLabel = "Stopping & finalising…"
        subStatusLabel = ""
        stopPolling()

        DispatchQueue.global(qos: .userInitiated).async {
            let result = DimmyCore.shared.meetingStop()
            DispatchQueue.main.async {
                self.isRecording = false
                self.isPulsing = false
                guard let result else {
                    self.isWorking = false
                    self.statusLabel = "Stop failed"
                    self.subStatusLabel = "See dimmy.log for details."
                    return
                }
                self.activeDir = result.dir
                self.transcript = result.transcript.isEmpty
                    ? "(no transcript — VAD may have removed all audio)"
                    : result.transcript
                self.statusLabel = "Done"
                self.subStatusLabel = String(
                    format: "%.0fs · %d chunks", result.durationSecs, result.chunkCount
                )
                self.chunkSummary = self.subStatusLabel

                if self.generateRecap, !result.transcript.trimmingCharacters(in: .whitespaces).isEmpty {
                    self.runPostProcess(dir: result.dir, transcript: result.transcript)
                } else {
                    self.isWorking = false
                }
            }
        }
    }

    private func runPostProcess(dir: String, transcript: String) {
        statusLabel = "Generating recap + actions via LLM…"
        let prompt = Self.buildPostProcessPrompt(transcript: transcript)
        let modelOverride = pickRecapModel()

        DispatchQueue.global(qos: .userInitiated).async {
            let result = DimmyCore.shared.llmCallRaw(
                prompt: prompt,
                modelOverride: modelOverride,
                maxTokens: 4096
            )
            DispatchQueue.main.async {
                self.isWorking = false
                switch result {
                case .success(let raw):
                    let (recap, actions) = Self.parsePostProcess(raw)
                    self.recap = recap
                    self.actions = actions
                    self.recapVisible = true
                    self.actionsVisible = true
                    DimmyCore.shared.meetingSavePostProcess(
                        dir: dir, recap: recap, actions: actions
                    )
                    self.statusLabel = "Recap saved"
                case .failure(let err):
                    self.statusLabel = "Recap failed"
                    self.subStatusLabel = "\(err)"
                }
            }
        }
    }

    // MARK: - Live transcript polling

    private func startPolling() {
        stopPolling()
        pollTimer = Timer.scheduledTimer(withTimeInterval: 2.0, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.tick() }
        }
        // Fire once immediately so the timer label starts at 00:00:00.
        tick()
    }

    private func stopPolling() {
        pollTimer?.invalidate()
        pollTimer = nil
    }

    private func tick() {
        // Update the timer label.
        if let started = startedAt {
            let secs = Int(Date().timeIntervalSince(started))
            let h = secs / 3600
            let m = (secs % 3600) / 60
            let s = secs % 60
            timerLabel = String(format: "%02d:%02d:%02d", h, m, s)
        }

        // Locate the meetings dir and read the most-recently-modified
        // session's transcripts.txt. We don't track the dir locally
        // because the Rust side picks it; the freshest mtime is
        // overwhelmingly the one we just started, with sub-second
        // precision more than enough to deduplicate.
        guard let baseDir = meetingsDir() else { return }
        let fm = FileManager.default
        guard let contents = try? fm.contentsOfDirectory(at: baseDir,
                                                          includingPropertiesForKeys: [.contentModificationDateKey],
                                                          options: [.skipsHiddenFiles])
        else { return }
        let latest = contents
            .filter { (try? $0.resourceValues(forKeys: [.isDirectoryKey]).isDirectory) == true }
            .compactMap { url -> (URL, Date)? in
                let date = (try? url.resourceValues(forKeys: [.contentModificationDateKey]).contentModificationDate) ?? .distantPast
                return (url, date)
            }
            .sorted { $0.1 > $1.1 }
            .first?.0
        guard let dir = latest else { return }
        activeDir = dir.path

        let transcriptsPath = dir.appendingPathComponent("transcripts.txt")
        if let content = try? String(contentsOf: transcriptsPath, encoding: .utf8),
           !content.isEmpty {
            transcript = content
            let chunkCount = content.split(separator: "\n", omittingEmptySubsequences: true).count
            chunkSummary = "\(chunkCount) chunk\(chunkCount == 1 ? "" : "s")"
        }
    }

    private func meetingsDir() -> URL? {
        guard let support = try? FileManager.default.url(
            for: .applicationSupportDirectory,
            in: .userDomainMask,
            appropriateFor: nil,
            create: false
        ) else { return nil }
        return support.appendingPathComponent("dimmy/meetings", isDirectory: true)
    }

    // MARK: - Prompt + parse helpers

    static func buildPostProcessPrompt(transcript: String) -> String {
        return """
        You are summarizing a meeting transcript. Output ONLY the two sections below, separated by the exact markers shown.

        ===RECAP===
        <3-6 bullet points covering decisions made, topics discussed, and outcomes>

        ===ACTIONS===
        <numbered list of action items in the form: N. owner — task — due (or 'unspecified')>

        Transcript:
        \(transcript)
        """
    }

    static func parsePostProcess(_ raw: String) -> (String, String) {
        let recapMarker = "===RECAP==="
        let actionsMarker = "===ACTIONS==="
        let lower = raw
        guard let rRange = lower.range(of: recapMarker, options: .caseInsensitive),
              let aRange = lower.range(of: actionsMarker, options: .caseInsensitive),
              aRange.lowerBound > rRange.upperBound else {
            return (raw.trimmingCharacters(in: .whitespacesAndNewlines), "")
        }
        let recap = String(raw[rRange.upperBound..<aRange.lowerBound])
            .trimmingCharacters(in: .whitespacesAndNewlines)
        let actions = String(raw[aRange.upperBound...])
            .trimmingCharacters(in: .whitespacesAndNewlines)
        return (recap, actions)
    }
}

// MARK: - MeetingWindowController
//
// Owns the meeting window. We use a plain NSWindow (not the floating
// pill panel) so the user can move it, resize it, and leave it visible
// alongside their meeting app of choice.

@MainActor
final class MeetingWindowController {
    static let shared = MeetingWindowController()

    private var window: NSWindow?
    let viewModel = MeetingViewModel()

    /// Show (or activate) the meeting window. If a meeting is in
    /// progress when the user calls this — say they closed the window
    /// mid-recording — the new window picks up the live state via the
    /// poll loop.
    func show() {
        if let window {
            window.makeKeyAndOrderFront(nil)
            NSApp.activate(ignoringOtherApps: true)
            return
        }

        let view = MeetingView(vm: viewModel)
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 720, height: 560),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.title = "Dimmy — Meeting"
        window.center()
        window.contentView = NSHostingView(rootView: view)
        window.isReleasedWhenClosed = false
        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
        self.window = window
    }
}

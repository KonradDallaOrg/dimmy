import SwiftUI
import AppKit
import UniformTypeIdentifiers

// MARK: - FileLoadCard
//
// Drag-drop a WAV onto the card or click "Choose file…" to pick one.
// We then call `dimmy_transcribe_file` on a background thread; the Rust
// core decodes via hound, runs the same preprocess pipeline as live
// recording (highpass + VAD + AGC + downsample to 16 k), routes per the
// active local backend (whisper.cpp / Parakeet), and saves to history.
//
// Progress arrives via `file_transcribe_progress` events on the global
// AppState publisher (see DimmyCore.swift event dispatcher). We bind a
// determinate progress bar to it; the chunk index/total drive the
// "Transcribing… X / Y s (Z%)" status text.

struct FileLoadCard: View {
    @ObservedObject var appState: AppState

    enum Status: Equatable {
        case idle
        case starting
        case transcribing
        case done(String)
        case error(String)
    }

    @State private var status: Status = .idle
    @State private var dropTargeted: Bool = false
    /// The path of the file we last asked Rust to transcribe — used so
    /// the "Reveal" button still works after the result lands.
    @State private var lastPath: String?

    // File-load → meeting recap state. Mirrors Win
    // SettingsWindow._lastFileLoad{Path,Transcript} + RecapPanel
    // visibility. The button only appears after a successful
    // transcript so we never recap a stale pair.
    @State private var recapRunning: Bool = false
    @State private var recapStatus: String? = nil
    @State private var recapDir: String? = nil

    var body: some View {
        MacTile {
            VStack(spacing: 0) {
                MacRow(
                    "Transcribe a file",
                    description: descriptionText,
                    icon: "waveform.badge.magnifyingglass",
                    iconBackground: Color(red: 0.34, green: 0.61, blue: 0.99),
                    showsDivider: false
                ) {
                    HStack(spacing: 8) {
                        Button {
                            chooseFile()
                        } label: {
                            Label("Choose file…", systemImage: "doc.badge.plus")
                        }
                        .controlSize(.small)
                        .disabled(isWorking)
                    }
                }

                progressOrResult
                    .padding(.horizontal, 14)
                    .padding(.bottom, 14)
            }
        }
        .overlay {
            // Drop highlight — accent border + subtle wash. Falls back
            // gracefully when the user drags something other than a WAV.
            if dropTargeted {
                RoundedRectangle(cornerRadius: MacTheme.tileCornerRadius, style: .continuous)
                    .stroke(Color.accentColor, lineWidth: 1.5)
                    .background(
                        RoundedRectangle(cornerRadius: MacTheme.tileCornerRadius, style: .continuous)
                            .fill(Color.accentColor.opacity(0.06))
                    )
                    .allowsHitTesting(false)
            }
        }
        .onDrop(of: [.fileURL], isTargeted: $dropTargeted) { providers in
            handleDrop(providers: providers)
        }
        .onChange(of: appState.fileTranscribeProgress) { _, progress in
            if status == .starting && progress != nil {
                status = .transcribing
            }
        }
    }

    private var descriptionText: String {
        if isWorking { return "Working… leave the page open." }
        return "Drag a .wav onto this card, or click Choose file. Routed through your active local STT backend."
    }

    private var isWorking: Bool {
        switch status {
        case .starting, .transcribing: return true
        default: return false
        }
    }

    @ViewBuilder
    private var progressOrResult: some View {
        switch status {
        case .idle:
            EmptyView()
        case .starting:
            HStack(spacing: 8) {
                ProgressView().controlSize(.small).scaleEffect(0.7)
                Text("Starting…")
                    .font(.system(size: 11))
                    .foregroundStyle(Color.macTextSecondary)
            }
        case .transcribing:
            VStack(alignment: .leading, spacing: 4) {
                if let p = appState.fileTranscribeProgress {
                    ProgressView(value: p.percent, total: 100)
                        .progressViewStyle(.linear)
                    Text(progressLabel(p))
                        .font(.system(size: 11))
                        .foregroundStyle(Color.macTextSecondary)
                } else {
                    HStack(spacing: 8) {
                        ProgressView().controlSize(.small).scaleEffect(0.7)
                        Text("Transcribing…")
                            .font(.system(size: 11))
                            .foregroundStyle(Color.macTextSecondary)
                    }
                }
            }
        case .done(let text):
            VStack(alignment: .leading, spacing: 4) {
                HStack(spacing: 6) {
                    Image(systemName: "checkmark.circle.fill")
                        .foregroundStyle(.green)
                    Text("Transcribed — saved to history")
                        .font(.system(size: 12, weight: .medium))
                    Spacer()
                    Button("Copy") {
                        NSPasteboard.general.clearContents()
                        NSPasteboard.general.setString(text, forType: .string)
                    }
                    .controlSize(.small)
                    if let p = lastPath {
                        Button("Reveal") {
                            NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: p)])
                        }
                        .controlSize(.small)
                    }
                }
                ScrollView {
                    Text(text)
                        .font(.system(size: 11))
                        .foregroundStyle(Color.primary)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .textSelection(.enabled)
                }
                .frame(maxHeight: 80)
                .padding(8)
                .background(Color.gray.opacity(0.08))
                .cornerRadius(6)

                // Run-recap-as-meeting bridge. Same UX as the Win
                // "Run recap as meeting" button in
                // SettingsWindow.xaml — mints a synthetic meeting
                // dir with the WAV + transcript, fires the same LLM
                // recap pipeline used by live meetings. Result shows
                // up in Meeting → History.
                HStack(spacing: 8) {
                    Button {
                        runRecapAsMeeting(transcript: text)
                    } label: {
                        Label("Run recap as meeting", systemImage: "doc.text.magnifyingglass")
                    }
                    .controlSize(.small)
                    .disabled(recapRunning)

                    if recapRunning {
                        ProgressView().controlSize(.small).scaleEffect(0.7)
                    }
                    if let s = recapStatus {
                        Text(s)
                            .font(.system(size: 11))
                            .foregroundStyle(Color.macTextSecondary)
                            .lineLimit(2)
                    }
                    Spacer()
                }
                .padding(.top, 4)
            }
        case .error(let msg):
            HStack(spacing: 6) {
                Image(systemName: "exclamationmark.triangle.fill")
                    .foregroundStyle(.orange)
                Text(msg)
                    .font(.system(size: 11))
                    .foregroundStyle(Color.macTextSecondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    private func progressLabel(_ p: FileTranscribeProgress) -> String {
        if p.chunkTotal > 0 {
            return String(
                format: "Chunk %d / %d · %.0f%% · %.0fs of %.0fs",
                p.chunkIndex, p.chunkTotal, p.percent, p.processedSecs, p.totalSecs
            )
        }
        return String(format: "%.0f%% · %.0fs of %.0fs", p.percent, p.processedSecs, p.totalSecs)
    }

    // MARK: - Drag-drop handling

    private func handleDrop(providers: [NSItemProvider]) -> Bool {
        guard !isWorking else { return false }
        var handled = false
        for provider in providers {
            if provider.hasItemConformingToTypeIdentifier(UTType.fileURL.identifier) {
                handled = true
                _ = provider.loadObject(ofClass: URL.self) { url, _ in
                    DispatchQueue.main.async {
                        guard let url = url else { return }
                        if !isAcceptableAudioPath(url.path) {
                            status = .error("Drag a .wav file. mp3/m4a support is on the roadmap.")
                            return
                        }
                        run(path: url.path)
                    }
                }
                break  // only handle the first dropped item
            }
        }
        return handled
    }

    // MARK: - Open panel

    private func chooseFile() {
        let panel = NSOpenPanel()
        panel.allowsMultipleSelection = false
        panel.canChooseDirectories = false
        panel.canChooseFiles = true
        // .audio covers a broad set; we still gate by extension below
        // because dimmy_transcribe_file only decodes WAV today.
        panel.allowedContentTypes = [.wav, .audio]
        panel.message = "Pick a WAV file to transcribe"
        if panel.runModal() == .OK, let url = panel.url {
            if !isAcceptableAudioPath(url.path) {
                status = .error("Only .wav is supported today.")
                return
            }
            run(path: url.path)
        }
    }

    private func isAcceptableAudioPath(_ path: String) -> Bool {
        path.lowercased().hasSuffix(".wav")
    }

    // MARK: - Run recap as meeting

    /// "Run recap as meeting" — promotes the last successful
    /// file-load transcript into a synthetic meeting dir + runs the
    /// same LLM recap pipeline live meetings use. Result lands in
    /// Meeting → History.
    ///
    /// The LLM call (`MeetingPostProcessService.runRecap`) can take
    /// 10–60 s — runs on a background queue so the SwiftUI thread
    /// stays responsive. Button stays disabled until completion to
    /// prevent double-fires.
    private func runRecapAsMeeting(transcript: String) {
        guard !recapRunning else { return }
        recapRunning = true
        recapStatus = "Building meeting + running recap…"
        recapDir = nil

        let path = lastPath ?? ""
        let transcriptCopy = transcript
        // Capture the @MainActor-isolated notionAutoSend flag here on
        // the main thread so the background dispatch doesn't need to
        // touch AppState.shared.notionAutoSend — Swift 6 rejects that
        // cross-isolation read as a hard error.
        let notionAutoSend = AppState.shared.notionAutoSend
        DispatchQueue.global(qos: .userInitiated).async {
            let outcome = FileLoadToMeetingService.run(
                sourceWavPath: path,
                transcript: transcriptCopy,
                notionAutoSend: notionAutoSend
            )
            DispatchQueue.main.async {
                recapRunning = false
                if outcome.success {
                    recapDir = outcome.dir
                    recapStatus = "✓ Recap saved. Open Meeting → History."
                    NSLog("[FileLoadCard] recap success at \(outcome.dir)")
                } else {
                    recapStatus = "Failed: \(outcome.error ?? "unknown")"
                    NSLog("[FileLoadCard] recap failed: \(outcome.error ?? "nil")")
                }
            }
        }
    }

    // MARK: - Run

    private func run(path: String) {
        // Cloud STT is unsupported via this entry point — fail fast
        // with a clear hint instead of waiting for the -4 return.
        if appState.sttMode != "local" {
            status = .error("File-load works only with a local STT backend. Switch to local in Voice → Speech-to-text.")
            return
        }

        lastPath = path
        status = .starting
        appState.fileTranscribeProgress = nil

        DispatchQueue.global(qos: .userInitiated).async {
            // Lazy init for users who skipped mic permission. dimmy_init is
            // idempotent (returns 1 = "already inited"), so this is safe to
            // call from any feature path — file load doesn't need a mic.
            if !DimmyCore.shared.isInitialized {
                _ = DimmyCore.shared.initialize()
            }
            if !DimmyCore.shared.isInitialized {
                DispatchQueue.main.async {
                    status = .error("Dimmy core failed to initialize. Check Permissions and dimmy.log.")
                }
                return
            }
            let result = DimmyCore.shared.transcribeFile(at: path)
            DispatchQueue.main.async {
                appState.fileTranscribeProgress = nil
                switch result {
                case .success(let text):
                    if text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                        status = .error("The local backend produced an empty transcript.")
                    } else {
                        status = .done(text)
                    }
                case .failure(let err):
                    status = .error("\(err)")
                }
            }
        }
    }
}

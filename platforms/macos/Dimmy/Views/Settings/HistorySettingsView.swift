import SwiftUI
import AppKit
import AVFoundation

/// History, past dictations grouped by date, in the Tahoe design
/// language (MacTile + MacRow). v2 fields surfaced: enhanced_text
/// (Raw/Enhanced toggle in the detail sheet), audio_path (waveform
/// playback), app_bundle_id (real Mac icon + display name),
/// size_bytes, llm_style, llm_translate_to.
struct HistorySettingsView: View {
    @ObservedObject var appState: AppState
    @State private var searchQuery: String = ""
    @State private var transcripts: [HistoryEntry] = []
    @State private var stats: [String: Any]?
    @State private var selected: HistoryEntry?
    @State private var refreshTimer: Timer?

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            searchField
                .padding(.bottom, 8)

            if let stats = stats {
                statsTile(stats)
                    .padding(.bottom, 12)
            }

            // Audio retention controls. Moved from Privacy & data
            // per docs/dev/settings-redesign-checklist.md: the toggle
            // and its nested retention/quota knobs belong on
            // Recordings because that is where the saved audio
            // actually plays back.
            audioRetentionGroup
                .padding(.bottom, 12)

            if transcripts.isEmpty {
                emptyState
            } else {
                transcriptGroups
            }
        }
        .onAppear {
            loadTranscripts()
            loadStats()
            refreshTimer = Timer.scheduledTimer(withTimeInterval: 5.0, repeats: true) { _ in
                Task { @MainActor in
                    self.loadTranscripts()
                    self.loadStats()
                }
            }
        }
        .onDisappear {
            refreshTimer?.invalidate()
            refreshTimer = nil
        }
        .sheet(item: $selected) { entry in
            HistoryDetailSheet(
                entry: entry,
                onDelete: { deleteTranscript(id: $0) }
            )
        }
    }

    // MARK: - Audio retention

    private var audioRetentionGroup: some View {
        Group {
            MacGroupLabel(text: "Audio retention")
            MacTile {
                MacRow(
                    "Save audio with each recording",
                    description: "Off by default.",
                    hint: "When on, each transcription's audio file is saved alongside the row at 16 kHz mono so you can play it back from the detail sheet. Use the retention controls below to bound disk usage.",
                    hintURL: URL(string: "https://dimmy.app/help/history-search")
                ) {
                    Toggle("", isOn: Binding(
                        get: { appState.saveAudioInHistory },
                        set: { newValue in
                            appState.saveAudioInHistory = newValue
                            DimmyCore.shared.setConfig(appState.toRustConfig())
                        }
                    ))
                    .toggleStyle(.switch)
                    .labelsHidden()
                }
                MacRow(
                    "Keep for",
                    description: "0 = never auto-delete by age.",
                    hint: "Days before a saved audio file is automatically deleted from the history_audio folder. Does not affect the recording row itself, only the audio file.",
                    hintURL: URL(string: "https://dimmy.app/help/history-search")
                ) {
                    Stepper(value: Binding(
                        get: { Int(appState.historyAudioKeepDays) },
                        set: { appState.historyAudioKeepDays = UInt32(max(0, $0))
                               DimmyCore.shared.setConfig(appState.toRustConfig()) }
                    ), in: 0...365) {
                        Text("\(appState.historyAudioKeepDays) day\(appState.historyAudioKeepDays == 1 ? "" : "s")")
                            .font(.system(size: 12))
                    }
                    .controlSize(.small)
                    .disabled(!appState.saveAudioInHistory)
                }
                MacRow(
                    "Storage cap",
                    description: "0 = no cap.",
                    hint: "Maximum size of the history_audio folder in megabytes. Oldest audio files are deleted first when over the cap, even if they are still within the Keep-for window.",
                    hintURL: URL(string: "https://dimmy.app/help/history-search"),
                    showsDivider: false
                ) {
                    Stepper(value: Binding(
                        get: { Int(appState.historyAudioMaxMb) },
                        set: { appState.historyAudioMaxMb = UInt32(max(0, $0))
                               DimmyCore.shared.setConfig(appState.toRustConfig()) }
                    ), in: 0...50_000, step: 100) {
                        Text("\(appState.historyAudioMaxMb) MB")
                            .font(.system(size: 12))
                    }
                    .controlSize(.small)
                    .disabled(!appState.saveAudioInHistory)
                }
            }
        }
    }

    // MARK: - Search bar

    private var searchField: some View {
        MacTile {
            HStack(spacing: 8) {
                Image(systemName: "magnifyingglass")
                    .font(.system(size: 12))
                    .foregroundStyle(Color.macTextSecondary)
                TextField("Search transcripts...", text: $searchQuery)
                    .textFieldStyle(.plain)
                    .font(.system(size: 13))
                    .onChange(of: searchQuery) { _, _ in loadTranscripts() }
                if !searchQuery.isEmpty {
                    Button {
                        searchQuery = ""
                    } label: {
                        Image(systemName: "xmark.circle.fill")
                            .foregroundStyle(Color.macTextTertiary)
                    }
                    .buttonStyle(.plain)
                }
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 10)
        }
    }

    // MARK: - Stats tile

    private func statsTile(_ stats: [String: Any]) -> some View {
        let totalWords = stats["total_words"] as? Int ?? 0
        let totalSessions = stats["total_sessions"] as? Int ?? 0
        let totalDuration = stats["total_duration"] as? Double ?? 0.0
        return HStack(spacing: 8) {
            MacStatTile(value: "\(totalSessions)", label: "Sessions")
            MacStatTile(value: "\(totalWords.formatted())", label: "Words")
            MacStatTile(value: formatDuration(totalDuration), label: "Total time")
        }
    }

    // MARK: - Empty state

    private var emptyState: some View {
        MacTile {
            VStack(spacing: 10) {
                Image(systemName: searchQuery.isEmpty ? "waveform.path" : "magnifyingglass")
                    .font(.system(size: 36))
                    .foregroundStyle(Color.macTextTertiary)
                Text(searchQuery.isEmpty ? "No transcriptions yet" : "No results for \"\(searchQuery)\"")
                    .font(.system(size: 13, weight: .medium))
                if searchQuery.isEmpty {
                    Text("Press your shortcut anywhere or drop an audio file on Home. Recordings land here.")
                        .font(.system(size: 11))
                        .foregroundStyle(Color.macTextSecondary)
                        .multilineTextAlignment(.center)
                }
            }
            .frame(maxWidth: .infinity)
            .padding(.vertical, 36)
            .padding(.horizontal, 20)
        }
    }

    // MARK: - Grouped list

    private var transcriptGroups: some View {
        VStack(alignment: .leading, spacing: 0) {
            ForEach(groupedTranscripts, id: \.0) { groupName, items in
                MacGroupLabel(text: "\(groupName) · \(items.count)")
                MacTile {
                    VStack(spacing: 0) {
                        ForEach(Array(items.enumerated()), id: \.element.id) { idx, entry in
                            row(entry)
                            if idx < items.count - 1 {
                                Divider()
                                    .background(Color.macRowDivider)
                                    .padding(.leading, 14)
                                    .opacity(0.6)
                            }
                        }
                    }
                }
                .padding(.bottom, 8)
            }
        }
    }

    @ViewBuilder
    private func row(_ entry: HistoryEntry) -> some View {
        Button {
            selected = entry
        } label: {
            HStack(spacing: 12) {
                appBadge(entry)

                VStack(alignment: .leading, spacing: 4) {
                    Text(entry.preview)
                        .font(.system(size: 13))
                        .foregroundStyle(Color.primary)
                        .lineLimit(2)
                        .multilineTextAlignment(.leading)
                        .frame(maxWidth: .infinity, alignment: .leading)

                    HStack(spacing: 6) {
                        meta(formatTimestamp(entry.timestamp), systemImage: "clock")
                        if entry.duration > 0 {
                            meta(formatShortDuration(entry.duration), systemImage: "waveform")
                        }
                        if !entry.language.isEmpty {
                            tag(entry.language.uppercased(), color: .accentColor)
                        }
                        if entry.hasEnhanced {
                            tag("Enhanced", color: .purple, systemImage: "sparkles")
                        }
                        if entry.hasAudio {
                            tag(formatBytesShort(entry.sizeBytes), color: .blue, systemImage: "play.circle.fill")
                        }
                        Spacer()
                    }
                }

                Image(systemName: "chevron.right")
                    .font(.system(size: 10, weight: .semibold))
                    .foregroundStyle(Color.macTextTertiary)
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 12)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .contextMenu {
            Button {
                NSPasteboard.general.clearContents()
                NSPasteboard.general.setString(entry.preferredDisplay, forType: .string)
            } label: {
                Label("Copy", systemImage: "doc.on.doc")
            }
            if entry.hasEnhanced {
                Button {
                    NSPasteboard.general.clearContents()
                    NSPasteboard.general.setString(entry.text, forType: .string)
                } label: {
                    Label("Copy raw", systemImage: "doc.plaintext")
                }
            }
            if let p = entry.audioPath, !p.isEmpty {
                Button {
                    NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: p)])
                } label: {
                    Label("Reveal audio", systemImage: "folder")
                }
            }
            Divider()
            Button(role: .destructive) {
                deleteTranscript(id: entry.id)
            } label: {
                Label("Delete", systemImage: "trash")
            }
        }
    }

    @ViewBuilder
    private func appBadge(_ entry: HistoryEntry) -> some View {
        ZStack {
            // Subtle squircle backdrop so the icon visually anchors
            // (and lines up with the rest of the Tahoe layout) even
            // when the .icns is just a flat glyph.
            RoundedRectangle(cornerRadius: 9, style: .continuous)
                .fill(Color.primary.opacity(0.12))
                .frame(width: 36, height: 36)

            if let bundleId = entry.appBundleId,
               !bundleId.isEmpty,
               let icon = AppContextCapture.appIcon(for: bundleId) {
                Image(nsImage: icon)
                    .resizable()
                    .interpolation(.high)
                    .aspectRatio(contentMode: .fit)
                    .frame(width: 28, height: 28)
            } else {
                Image(systemName: "waveform")
                    .font(.system(size: 14, weight: .semibold))
                    .foregroundStyle(Color.accentColor)
            }
        }
    }

    private func meta(_ text: String, systemImage: String) -> some View {
        HStack(spacing: 3) {
            Image(systemName: systemImage)
                .font(.system(size: 9))
            Text(text)
                .font(.system(size: 10))
        }
        .foregroundStyle(Color.macTextSecondary)
    }

    private func tag(_ text: String, color: Color, systemImage: String? = nil) -> some View {
        HStack(spacing: 3) {
            if let img = systemImage {
                Image(systemName: img)
                    .font(.system(size: 9, weight: .semibold))
            }
            Text(text)
                .font(.system(size: 10, weight: .semibold))
        }
        .foregroundStyle(color)
        .padding(.horizontal, 6)
        .padding(.vertical, 2)
        .background(color.opacity(0.14))
        .clipShape(Capsule())
    }

    // MARK: - Data loading

    func loadTranscripts() {
        let raw: [[String: Any]]
        if searchQuery.isEmpty {
            raw = DimmyCore.shared.historyRecent(limit: 200) ?? []
        } else {
            raw = DimmyCore.shared.historySearch(query: searchQuery, limit: 200) ?? []
        }
        transcripts = raw.map { HistoryEntry(dict: $0) }
    }

    func loadStats() {
        stats = DimmyCore.shared.historyStats()
    }

    func deleteTranscript(id: Int32) {
        _ = DimmyCore.shared.historyDelete(id: id)
        if selected?.id == id { selected = nil }
        loadTranscripts()
        loadStats()
    }

    // MARK: - Date grouping

    private var groupedTranscripts: [(String, [HistoryEntry])] {
        let grouped = Dictionary(grouping: transcripts) { entry in
            dateGroup(for: entry.timestamp)
        }
        let groupOrder = ["Today", "Yesterday", "This Week", "Older"]
        return groupOrder.compactMap { group in
            guard let items = grouped[group], !items.isEmpty else { return nil }
            let sorted = items.sorted { $0.timestamp > $1.timestamp }
            return (group, sorted)
        }
    }

    private func dateGroup(for timestamp: Double) -> String {
        let date = Date(timeIntervalSince1970: timestamp)
        let calendar = Calendar.current
        if calendar.isDateInToday(date) { return "Today" }
        if calendar.isDateInYesterday(date) { return "Yesterday" }
        if let weekAgo = calendar.date(byAdding: .day, value: -7, to: Date()),
           date > weekAgo { return "This Week" }
        return "Older"
    }

    // MARK: - Formatting

    private func formatTimestamp(_ timestamp: Double) -> String {
        let date = Date(timeIntervalSince1970: timestamp)
        let calendar = Calendar.current
        let timeFmt = DateFormatter()
        timeFmt.dateFormat = "HH:mm"
        if calendar.isDateInToday(date) { return timeFmt.string(from: date) }
        if calendar.isDateInYesterday(date) { return "Yesterday \(timeFmt.string(from: date))" }
        let dayFmt = DateFormatter()
        dayFmt.dateStyle = .medium
        dayFmt.timeStyle = .short
        return dayFmt.string(from: date)
    }

    private func formatShortDuration(_ secs: Double) -> String {
        if secs < 60 { return String(format: "%.0fs", secs) }
        let m = Int(secs) / 60
        let s = Int(secs) % 60
        return s == 0 ? "\(m)m" : "\(m)m \(s)s"
    }

    private func formatDuration(_ secs: Double) -> String {
        guard secs > 0 else { return "0s" }
        let h = Int(secs) / 3600
        let m = (Int(secs) % 3600) / 60
        let s = Int(secs) % 60
        if h > 0 { return String(format: "%dh %02dm", h, m) }
        if m > 0 { return String(format: "%dm %02ds", m, s) }
        return String(format: "%ds", s)
    }

    fileprivate static func formatBytesShort(_ bytes: Int64) -> String {
        let fmt = ByteCountFormatter()
        fmt.allowedUnits = [.useKB, .useMB]
        fmt.countStyle = .file
        return fmt.string(fromByteCount: bytes)
    }

    private func formatBytesShort(_ bytes: Int64) -> String { Self.formatBytesShort(bytes) }
}

// MARK: - HistoryEntry

struct HistoryEntry: Identifiable, Equatable {
    let id: Int32
    let text: String
    let enhancedText: String?
    let language: String
    let timestamp: Double
    let duration: Double
    let wordCount: Int
    let audioPath: String?
    let appBundleId: String?
    let appProcessName: String?
    let llmStyle: String?
    let llmTranslateTo: String?
    let sizeBytes: Int64

    init(dict: [String: Any]) {
        self.id = (dict["id"] as? Int32) ?? Int32(dict["id"] as? Int ?? 0)
        self.text = dict["text"] as? String ?? ""
        self.enhancedText = dict["enhanced_text"] as? String
        self.language = dict["language"] as? String ?? ""
        self.timestamp = dict["timestamp"] as? Double ?? 0.0
        self.duration = dict["duration"] as? Double ?? 0.0
        self.wordCount = dict["word_count"] as? Int ?? 0
        self.audioPath = dict["audio_path"] as? String
        self.appBundleId = dict["app_bundle_id"] as? String
        self.appProcessName = dict["app_process_name"] as? String
        self.llmStyle = dict["llm_style"] as? String
        self.llmTranslateTo = dict["llm_translate_to"] as? String
        if let sb = dict["size_bytes"] as? Int64 {
            self.sizeBytes = sb
        } else if let sb = dict["size_bytes"] as? Int {
            self.sizeBytes = Int64(sb)
        } else {
            self.sizeBytes = 0
        }
    }

    var hasEnhanced: Bool {
        guard let e = enhancedText, !e.isEmpty else { return false }
        return e != text
    }

    var hasAudio: Bool {
        guard let p = audioPath, !p.isEmpty else { return false }
        return FileManager.default.fileExists(atPath: p)
    }

    var preferredDisplay: String {
        if let e = enhancedText, !e.isEmpty { return e }
        return text
    }

    var preview: String {
        let s = preferredDisplay
            .replacingOccurrences(of: "\n", with: " ")
            .replacingOccurrences(of: "\r", with: " ")
            .trimmingCharacters(in: .whitespacesAndNewlines)
        if s.count > 110 { return String(s.prefix(110)) + "..." }
        return s
    }

    var appDisplay: String {
        if let id = appBundleId, !id.isEmpty {
            let resolved = AppContextCapture.appName(for: id)
            return resolved.isEmpty ? id : resolved
        }
        if let n = appProcessName, !n.isEmpty { return n }
        return ""
    }
}

// MARK: - Detail sheet (Tahoe-style)

struct HistoryDetailSheet: View {
    let entry: HistoryEntry
    let onDelete: (Int32) -> Void

    @Environment(\.dismiss) private var dismiss
    @State private var showEnhanced: Bool

    init(entry: HistoryEntry, onDelete: @escaping (Int32) -> Void) {
        self.entry = entry
        self.onDelete = onDelete
        self._showEnhanced = State(initialValue: entry.hasEnhanced)
    }

    var body: some View {
        VStack(spacing: 0) {
            header
                .padding(.horizontal, 18)
                .padding(.top, 16)
                .padding(.bottom, 12)

            ScrollView {
                VStack(alignment: .leading, spacing: 14) {
                    if entry.hasAudio, let path = entry.audioPath {
                        MacTile {
                            AudioWaveformView(path: path)
                                .padding(.horizontal, 14)
                                .padding(.vertical, 12)
                        }
                    }

                    if entry.hasEnhanced {
                        Picker("", selection: $showEnhanced) {
                            Label("Enhanced", systemImage: "sparkles").tag(true)
                            Label("Raw", systemImage: "doc.plaintext").tag(false)
                        }
                        .pickerStyle(.segmented)
                        .labelsHidden()
                    }

                    MacTile {
                        Text(currentText.isEmpty ? "(empty transcript)" : currentText)
                            .font(.system(size: 13))
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .textSelection(.enabled)
                            .padding(.horizontal, 14)
                            .padding(.vertical, 12)
                    }

                    metadataTile
                }
                .padding(.horizontal, 18)
                .padding(.bottom, 18)
            }
        }
        .frame(minWidth: 560, minHeight: 460)
        .background(.regularMaterial)
    }

    // MARK: Header

    private var header: some View {
        HStack(alignment: .center, spacing: 12) {
            ZStack {
                RoundedRectangle(cornerRadius: 12, style: .continuous)
                    .fill(Color.primary.opacity(0.12))
                    .frame(width: 48, height: 48)
                if let bid = entry.appBundleId, !bid.isEmpty,
                   let icon = AppContextCapture.appIcon(for: bid) {
                    Image(nsImage: icon)
                        .resizable()
                        .interpolation(.high)
                        .aspectRatio(contentMode: .fit)
                        .frame(width: 38, height: 38)
                } else {
                    Image(systemName: "waveform")
                        .font(.system(size: 20, weight: .semibold))
                        .foregroundStyle(Color.accentColor)
                }
            }

            VStack(alignment: .leading, spacing: 2) {
                Text(entry.appDisplay.isEmpty ? "Dictation" : entry.appDisplay)
                    .font(.system(size: 16, weight: .semibold))
                Text(headerMeta)
                    .font(.system(size: 11))
                    .foregroundStyle(Color.macTextSecondary)
            }

            Spacer()

            Button {
                NSPasteboard.general.clearContents()
                NSPasteboard.general.setString(currentText, forType: .string)
            } label: {
                Label("Copy", systemImage: "doc.on.doc")
            }
            .controlSize(.small)

            Button(role: .destructive) {
                onDelete(entry.id)
                dismiss()
            } label: {
                Image(systemName: "trash")
                    .foregroundStyle(.red)
            }
            .buttonStyle(.borderless)
            .controlSize(.small)

            Button("Done") { dismiss() }
                .keyboardShortcut(.defaultAction)
                .controlSize(.small)
        }
    }

    private var currentText: String {
        if showEnhanced, let e = entry.enhancedText, !e.isEmpty { return e }
        return entry.text
    }

    private var headerMeta: String {
        let date = Date(timeIntervalSince1970: entry.timestamp)
        let fmt = DateFormatter()
        fmt.dateStyle = .medium
        fmt.timeStyle = .short
        var parts: [String] = [fmt.string(from: date)]
        if entry.duration > 0 {
            parts.append(String(format: "%.1fs", entry.duration))
        }
        if entry.wordCount > 0 {
            parts.append("\(entry.wordCount) words")
        }
        return parts.joined(separator: " · ")
    }

    // MARK: Metadata tile

    @ViewBuilder
    private var metadataTile: some View {
        let style = (entry.llmStyle ?? "")
        let translate = (entry.llmTranslateTo ?? "")
        if !style.isEmpty || !translate.isEmpty || (entry.appBundleId ?? "").isEmpty == false {
            MacGroupLabel(text: "Details")
            MacTile {
                VStack(spacing: 0) {
                    if let bid = entry.appBundleId, !bid.isEmpty {
                        MacRow(
                            "App",
                            description: bid,
                            icon: "app.fill",
                            iconBackground: Color(red: 0.34, green: 0.61, blue: 0.99)
                        ) {
                            Text(entry.appDisplay)
                                .font(.system(size: 12))
                                .foregroundStyle(Color.macTextSecondary)
                        }
                    }
                    if !style.isEmpty {
                        MacRow(
                            "Rewrite style",
                            icon: "wand.and.stars",
                            iconBackground: Color(red: 0.69, green: 0.32, blue: 0.87)
                        ) {
                            HStack(spacing: 4) {
                                Circle()
                                    .fill(MacStyleColor.color(for: style))
                                    .frame(width: 6, height: 6)
                                Text(style.capitalized)
                                    .font(.system(size: 12))
                            }
                        }
                    }
                    if !translate.isEmpty, translate != "none" {
                        MacRow(
                            "Translated to",
                            icon: "globe",
                            iconBackground: Color(red: 0.10, green: 0.69, blue: 0.45),
                            showsDivider: false
                        ) {
                            Text(translate.uppercased())
                                .font(.system(size: 12))
                        }
                    }
                }
            }
        }
    }
}

import SwiftUI
import AppKit

struct HistorySettingsView: View {
    @ObservedObject var appState: AppState
    @State private var searchQuery: String = ""
    @State private var transcripts: [[String: Any]] = []
    @State private var stats: [String: Any]?

    var body: some View {
        VStack(spacing: 0) {
            // Search bar
            HStack(spacing: 6) {
                Image(systemName: "magnifyingglass")
                    .foregroundColor(.secondary)
                TextField("Search transcripts...", text: $searchQuery)
                    .textFieldStyle(.plain)
                    .font(.system(.body))
                    .onChange(of: searchQuery) { _, _ in loadTranscripts() }
                if !searchQuery.isEmpty {
                    Button {
                        searchQuery = ""
                    } label: {
                        Image(systemName: "xmark.circle.fill")
                            .foregroundColor(.secondary)
                    }
                    .buttonStyle(.plain)
                }
            }
            .padding(8)

            Divider()

            // Transcript list or empty state
            if transcripts.isEmpty {
                emptyState
            } else {
                transcriptList
            }

            // Stats footer
            if let stats = stats {
                Divider()
                statsFooter(stats)
            }
        }
        .onAppear {
            loadTranscripts()
            loadStats()
        }
    }

    // MARK: - Empty State

    private var emptyState: some View {
        VStack(spacing: 12) {
            Spacer()
            Image(systemName: "waveform.slash")
                .font(.system(size: 40))
                .foregroundColor(.secondary)
            Text(searchQuery.isEmpty ? "No transcriptions yet" : "No results for \"\(searchQuery)\"")
                .font(.system(.body))
                .foregroundColor(.secondary)
            if searchQuery.isEmpty {
                Text("Start dictating to build your history")
                    .font(.system(size: 12))
                    .foregroundColor(Color(nsColor: .tertiaryLabelColor))
            }
            Spacer()
        }
        .frame(maxWidth: .infinity, minHeight: 200)
    }

    // MARK: - Transcript List (grouped by date)

    private var transcriptList: some View {
        List {
            ForEach(groupedTranscripts, id: \.0) { group, items in
                Section(header: Text(group)) {
                    ForEach(items, id: \.id) { entry in
                        transcriptRow(entry)
                            .contentShape(Rectangle())
                            .onTapGesture {
                                copyToClipboard(entry.text)
                            }
                            .contextMenu {
                                Button {
                                    copyToClipboard(entry.text)
                                } label: {
                                    Label("Copy", systemImage: "doc.on.doc")
                                }
                                Divider()
                                Button(role: .destructive) {
                                    deleteTranscript(id: entry.id)
                                } label: {
                                    Label("Delete", systemImage: "trash")
                                }
                            }
                    }
                }
            }
        }
        .listStyle(.inset)
    }

    private func transcriptRow(_ entry: TranscriptEntry) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(entry.text)
                .font(.system(.body))
                .lineLimit(2)
                .foregroundColor(.primary)

            HStack(spacing: 8) {
                Text(formatTimestamp(entry.timestamp))
                    .font(.system(size: 11))
                    .foregroundColor(.secondary)

                if entry.wordCount > 0 {
                    Text("\(entry.wordCount) words")
                        .font(.system(size: 10, weight: .medium))
                        .foregroundColor(.secondary)
                        .padding(.horizontal, 6)
                        .padding(.vertical, 2)
                        .background(Color.secondary.opacity(0.12))
                        .clipShape(Capsule())
                }

                if !entry.language.isEmpty {
                    Text(entry.language.uppercased())
                        .font(.system(size: 10, weight: .semibold))
                        .foregroundColor(.accentColor)
                        .padding(.horizontal, 6)
                        .padding(.vertical, 2)
                        .background(Color.accentColor.opacity(0.12))
                        .clipShape(Capsule())
                }

                Spacer()
            }
        }
        .padding(.vertical, 2)
    }

    // MARK: - Stats Footer

    private func statsFooter(_ stats: [String: Any]) -> some View {
        HStack(spacing: 16) {
            let totalWords = stats["total_words"] as? Int ?? 0
            let totalSessions = stats["total_sessions"] as? Int ?? 0
            let totalDuration = stats["total_duration"] as? Double ?? 0.0

            statPill(icon: "text.word.spacing", value: "\(totalWords)", label: "words")
            statPill(icon: "waveform", value: "\(totalSessions)", label: "sessions")
            statPill(icon: "timer", value: formatDuration(totalDuration), label: "total")
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
    }

    private func statPill(icon: String, value: String, label: String) -> some View {
        HStack(spacing: 4) {
            Image(systemName: icon)
                .font(.system(size: 10))
                .foregroundColor(.secondary)
            Text(value)
                .font(.system(size: 11, weight: .semibold))
                .monospacedDigit()
            Text(label)
                .font(.system(size: 10))
                .foregroundColor(.secondary)
        }
    }

    // MARK: - Data Loading

    func loadTranscripts() {
        if searchQuery.isEmpty {
            transcripts = DimmyCore.shared.historyRecent(limit: 100) ?? []
        } else {
            transcripts = DimmyCore.shared.historySearch(query: searchQuery, limit: 100) ?? []
        }
    }

    func loadStats() {
        stats = DimmyCore.shared.historyStats()
    }

    func deleteTranscript(id: Int32) {
        _ = DimmyCore.shared.historyDelete(id: id)
        loadTranscripts()
        loadStats()
    }

    func copyToClipboard(_ text: String) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(text, forType: .string)
    }

    // MARK: - Date Grouping

    private var groupedTranscripts: [(String, [TranscriptEntry])] {
        let entries = transcripts.map { TranscriptEntry(dict: $0) }

        let grouped = Dictionary(grouping: entries) { entry in
            dateGroup(for: entry.timestamp)
        }

        // Sort groups in display order
        let groupOrder = ["Today", "Yesterday", "This Week", "Older"]
        return groupOrder.compactMap { group in
            guard let items = grouped[group], !items.isEmpty else { return nil }
            // Sort items within group by timestamp descending (newest first)
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
        let timeFormatter = DateFormatter()
        timeFormatter.dateFormat = "h:mm a"

        if calendar.isDateInToday(date) {
            return timeFormatter.string(from: date)
        }
        if calendar.isDateInYesterday(date) {
            return "Yesterday \(timeFormatter.string(from: date))"
        }

        let dateFormatter = DateFormatter()
        dateFormatter.dateStyle = .medium
        dateFormatter.timeStyle = .short
        return dateFormatter.string(from: date)
    }

    private func formatDuration(_ seconds: Double) -> String {
        guard seconds > 0 else { return "0s" }
        let h = Int(seconds) / 3600
        let m = (Int(seconds) % 3600) / 60
        let s = Int(seconds) % 60
        if h > 0 {
            return String(format: "%dh %02dm", h, m)
        } else if m > 0 {
            return String(format: "%dm %02ds", m, s)
        } else {
            return String(format: "%ds", s)
        }
    }
}

// MARK: - TranscriptEntry (typed wrapper around JSON dict)

private struct TranscriptEntry {
    let id: Int32
    let text: String
    let language: String
    let timestamp: Double
    let duration: Double
    let wordCount: Int

    init(dict: [String: Any]) {
        self.id = dict["id"] as? Int32 ?? Int32(dict["id"] as? Int ?? 0)
        self.text = dict["text"] as? String ?? ""
        self.language = dict["language"] as? String ?? ""
        self.timestamp = dict["timestamp"] as? Double ?? 0.0
        self.duration = dict["duration"] as? Double ?? 0.0
        self.wordCount = dict["word_count"] as? Int ?? 0
    }
}

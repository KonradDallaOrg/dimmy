import SwiftUI

struct LLMModelSettingsView: View {
    @ObservedObject var appState = AppState.shared
    @State private var models: [[String: Any]] = []

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Local LLM Model").font(.headline)

            if models.isEmpty {
                Text("Loading models...")
                    .foregroundColor(.secondary)
            } else {
                ForEach(models.indices, id: \.self) { index in
                    modelRow(models[index])
                    if index < models.count - 1 {
                        Divider()
                    }
                }
            }

            Text("LLM models enhance your transcriptions locally. Larger models produce better results but need more memory.")
                .font(.system(size: 11))
                .foregroundColor(.secondary)
                .padding(.top, 4)
        }
        .onAppear { loadModels() }
    }

    // MARK: - Model Row

    @ViewBuilder
    private func modelRow(_ model: [String: Any]) -> some View {
        let name = model["name"] as? String ?? "unknown"
        let filename = model["filename"] as? String ?? "unknown"
        let description = model["description"] as? String ?? ""
        let sizeMb = model["size_mb"] as? Int ?? 0
        let isDownloaded = model["downloaded"] as? Bool ?? false
        let isSelected = filename == appState.localLlmModel

        VStack(alignment: .leading, spacing: 4) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    HStack(spacing: 6) {
                        Text(name)
                            .font(.system(size: 13, weight: isSelected ? .semibold : .regular))
                        if isSelected && isDownloaded {
                            Text("Active")
                                .font(.system(size: 10, weight: .medium))
                                .padding(.horizontal, 6)
                                .padding(.vertical, 1)
                                .background(Color.accentColor.opacity(0.15))
                                .foregroundColor(.accentColor)
                                .cornerRadius(4)
                        }
                    }
                    Text(description)
                        .font(.system(size: 11))
                        .foregroundColor(.secondary)
                    Text(formatMb(sizeMb))
                        .font(.system(size: 10))
                        .foregroundColor(.secondary)
                }

                Spacer()

                if isDownloaded {
                    downloadedButton(filename: filename, isSelected: isSelected)
                } else if appState.isDownloadingLlmModel && isDownloadingThis(filename) {
                    downloadingIndicator
                } else {
                    downloadButton(filename: filename)
                }
            }

            if appState.isDownloadingLlmModel && isDownloadingThis(filename) && appState.llmModelDownloadProgress > 0 {
                ProgressView(value: appState.llmModelDownloadProgress, total: 1.0)
                    .progressViewStyle(.linear)
                Text("\(Int(appState.llmModelDownloadProgress * 100))%")
                    .font(.system(size: 10))
                    .foregroundColor(.secondary)
            }
        }
        .padding(.vertical, 2)
    }

    // MARK: - Buttons

    @ViewBuilder
    private func downloadedButton(filename: String, isSelected: Bool) -> some View {
        if isSelected {
            Image(systemName: "checkmark.circle.fill")
                .foregroundColor(.green)
                .font(.system(size: 18))
        } else {
            Button("Select") {
                appState.localLlmModel = filename
                syncConfigToRust()
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
        }
    }

    private var downloadingIndicator: some View {
        ProgressView()
            .controlSize(.small)
            .frame(width: 60)
    }

    private func downloadButton(filename: String) -> some View {
        Button("Download") {
            startDownload(filename)
        }
        .buttonStyle(.borderedProminent)
        .controlSize(.small)
        .disabled(appState.isDownloadingLlmModel)
    }

    // MARK: - Download

    @State private var downloadingFilename: String = ""

    private func isDownloadingThis(_ filename: String) -> Bool {
        downloadingFilename == filename
    }

    private func startDownload(_ filename: String) {
        appState.isDownloadingLlmModel = true
        appState.llmModelDownloadProgress = 0.0
        downloadingFilename = filename

        DispatchQueue.global(qos: .userInitiated).async {
            let success = DimmyCore.shared.downloadLLMModel(filename)

            DispatchQueue.main.async {
                appState.isDownloadingLlmModel = false
                appState.llmModelDownloadProgress = 0.0
                downloadingFilename = ""

                if success {
                    appState.localLlmModel = filename
                    syncConfigToRust()
                }
                loadModels()
            }
        }
    }

    // MARK: - Helpers

    private func loadModels() {
        models = DimmyCore.shared.listLLMModels() ?? []
    }

    private func syncConfigToRust() {
        // includeLlm:true — this view writes localLlmModel (selecting
        // which Gemma .gguf to use). Without the flag the
        // local_llm_model field is omitted and the picker's choice
        // never reaches disk.
        DimmyCore.shared.setConfig(appState.toRustConfig(includeLlm: true))
    }

    private func formatMb(_ mb: Int) -> String {
        if mb <= 0 { return "" }
        if mb >= 1024 {
            return String(format: "%.1f GB", Double(mb) / 1024.0)
        }
        return "\(mb) MB"
    }
}

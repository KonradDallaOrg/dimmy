import SwiftUI

struct ModelSettingsView: View {
    @ObservedObject var appState = AppState.shared
    @State private var models: [[String: Any]] = []

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Local Model").font(.headline)

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

            Text("Models are downloaded from Hugging Face and stored locally. Larger models are more accurate but use more memory and disk space.")
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
        let isSelected = filename == appState.localModel

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
                } else if appState.isDownloadingModel && isDownloadingThis(filename) {
                    downloadingIndicator
                } else {
                    downloadButton(filename: filename)
                }
            }

            // Progress bar when downloading this model
            if appState.isDownloadingModel && isDownloadingThis(filename) && appState.modelDownloadProgress > 0 {
                ProgressView(value: appState.modelDownloadProgress, total: 1.0)
                    .progressViewStyle(.linear)
                Text("\(Int(appState.modelDownloadProgress * 100))%")
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
                appState.localModel = filename
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
        .disabled(appState.isDownloadingModel)
    }

    // MARK: - Download

    @State private var downloadingFilename: String = ""

    private func isDownloadingThis(_ filename: String) -> Bool {
        downloadingFilename == filename
    }

    private func startDownload(_ filename: String) {
        appState.isDownloadingModel = true
        appState.modelDownloadProgress = 0.0
        downloadingFilename = filename

        DispatchQueue.global(qos: .userInitiated).async {
            let success = DimmyCore.shared.downloadModel(filename)

            DispatchQueue.main.async {
                appState.isDownloadingModel = false
                appState.modelDownloadProgress = 0.0
                downloadingFilename = ""

                if success {
                    appState.localModel = filename
                    syncConfigToRust()
                }
                loadModels()
            }
        }
    }

    // MARK: - Helpers

    private func loadModels() {
        models = DimmyCore.shared.listLocalModels() ?? []
    }

    private func syncConfigToRust() {
        DimmyCore.shared.setConfig(appState.toRustConfig())
    }

    /// Format megabytes into human-readable size string.
    private func formatMb(_ mb: Int) -> String {
        if mb <= 0 { return "" }
        if mb >= 1024 {
            return String(format: "%.1f GB", Double(mb) / 1024.0)
        }
        return "\(mb) MB"
    }
}

import SwiftUI

struct ModelDownloadStepView: View {
    @ObservedObject var appState: AppState
    let onContinue: () -> Void

    @State private var downloadState: DownloadState = .notStarted

    enum DownloadState {
        case notStarted
        case downloading
        case completed
        case skipped
    }

    var body: some View {
        VStack(spacing: 20) {
            Spacer()

            Image(systemName: "cpu")
                .font(.system(size: 48))
                .foregroundColor(.accentColor)

            Text("Local Speech Recognition")
                .font(.title2.bold())

            Text("Dimmy transcribes your voice locally on this Mac.\nNo internet required. Your audio never leaves this device.")
                .multilineTextAlignment(.center)
                .foregroundColor(.secondary)
                .font(.system(size: 13))
                .lineSpacing(4)

            // Model info card
            VStack(spacing: 8) {
                Text("Whisper Base Model")
                    .font(.headline)
                Text("78 MB download \u{2022} Good accuracy")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }
            .padding()
            .background(RoundedRectangle(cornerRadius: 8).fill(.quaternary))

            // Action area (changes based on state)
            switch downloadState {
            case .notStarted:
                Button("Download Model") { startDownload() }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.large)

            case .downloading:
                VStack(spacing: 8) {
                    ProgressView(value: appState.modelDownloadProgress, total: 1.0)
                        .frame(width: 200)
                    Text("\(Int(appState.modelDownloadProgress * 100))%")
                        .font(.caption)
                        .foregroundColor(.secondary)
                }

            case .completed:
                HStack(spacing: 8) {
                    Image(systemName: "checkmark.circle.fill")
                        .foregroundColor(.green)
                    Text("Model ready!")
                }
                .font(.headline)

            case .skipped:
                Text("You can download the model later in Settings.")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }

            Spacer()

            // Bottom buttons
            HStack {
                if downloadState == .notStarted {
                    Button("Skip for now") {
                        downloadState = .skipped
                    }
                    .buttonStyle(.plain)
                    .foregroundColor(.secondary)
                }

                Spacer()

                Button(action: onContinue) {
                    Text("Continue")
                        .font(.system(size: 15, weight: .semibold))
                        .frame(maxWidth: 220)
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
                .disabled(downloadState == .downloading)
            }

            Spacer().frame(height: 16)
        }
        .padding(.horizontal, 40)
        .onAppear {
            // Check if model already downloaded
            if DimmyCore.shared.modelExists("ggml-base-q8_0.bin") {
                downloadState = .completed
            }
        }
    }

    private func startDownload() {
        downloadState = .downloading
        appState.isDownloadingModel = true
        appState.modelDownloadProgress = 0.0

        DispatchQueue.global(qos: .userInitiated).async {
            let success = DimmyCore.shared.downloadModel("ggml-base-q8_0.bin")
            DispatchQueue.main.async {
                appState.isDownloadingModel = false
                downloadState = success ? .completed : .notStarted
            }
        }
    }
}

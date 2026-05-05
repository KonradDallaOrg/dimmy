import SwiftUI

struct ModelDownloadStepView: View {
    @ObservedObject var appState: AppState
    let onContinue: () -> Void

    @State private var downloadState: DownloadState = .notStarted

    /// Sentinel for the Parakeet entry. Same value used by the Settings
    /// page + the Windows onboarding (`ParakeetTag` constant).
    private static let parakeetTag = "parakeet:fp32"
    private static let defaultWhisper = "ggml-base-q8_0.bin"

    /// What the user picks here lands in appState.localSttBackend +
    /// appState.localModel. Default is whisper-base (78 MB) — the
    /// recommended first-run choice; Parakeet (2.5 GB) is opt-in.
    @State private var selection: String = defaultWhisper

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

            // Model picker — whisper variants + Parakeet sentinel.
            VStack(spacing: 10) {
                Picker("", selection: $selection) {
                    Text("Whisper Base · 78 MB (recommended)").tag(Self.defaultWhisper)
                    Text("Whisper Small · 466 MB").tag("ggml-small-q8_0.bin")
                    Text("Whisper Medium · 1.5 GB").tag("ggml-medium-q8_0.bin")
                    Text("Parakeet TDT v3 · 466 MB · Apple Neural Engine").tag(Self.parakeetTag)
                }
                .labelsHidden()
                .pickerStyle(.menu)
                .frame(width: 320)
                .disabled(downloadState == .downloading)

                Text(currentDescription)
                    .font(.caption)
                    .foregroundColor(.secondary)
                    .multilineTextAlignment(.center)
            }
            .padding()
            .background(RoundedRectangle(cornerRadius: 8).fill(.quaternary))

            // Action area (changes based on state)
            switch downloadState {
            case .notStarted:
                Button(downloadButtonLabel) { startDownload() }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.large)

            case .downloading:
                VStack(spacing: 8) {
                    ProgressView(value: currentProgress, total: 1.0)
                        .frame(width: 200)
                    Text("\(Int(currentProgress * 100))%")
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
            refreshFromCore()
            applyAutoPick()
        }
        .onChange(of: selection) {
            // If the new selection is already on disk, jump straight
            // to .completed — the user can Continue without re-downloading.
            refreshFromCore()
        }
    }

    /// Smart default: on Apple Silicon with >= 2 GB free disk we auto-
    /// pick Parakeet (Apple Neural Engine, 466 MB) and kick the
    /// download — the user just clicks Continue when it's done. With
    /// less disk we fall back to Whisper Base (78 MB). On a previously
    /// completed onboarding (model already on disk) we don't re-trigger
    /// anything; the user is past this step.
    private func applyAutoPick() {
        guard downloadState == .notStarted else { return }
        // Don't override an explicit choice the user already made by
        // tapping the picker — onAppear fires once on entry, so this
        // only runs on a fresh visit.
        let freeGB = availableDiskGB() ?? 0
        if freeGB >= 2 {
            // Bump the default to Parakeet and start downloading it.
            // The user can still pick a smaller model from the dropdown
            // if they cancel — the picker remains active.
            if selection != Self.parakeetTag {
                selection = Self.parakeetTag
            }
            // refreshFromCore on the new selection runs via .onChange;
            // if Parakeet is already on disk it'll flip to .completed
            // and we won't auto-start.
            DispatchQueue.main.async {
                if downloadState == .notStarted {
                    startDownload()
                }
            }
        } else if freeGB >= 0.2 {
            if selection != Self.defaultWhisper {
                selection = Self.defaultWhisper
            }
        }
        // < 200 MB free: stay on the .notStarted UI so the user has to
        // make space and click Download manually. We don't show a
        // dedicated error here — the model dropdown plus the implicit
        // OS "Disk full" notification is enough.
    }

    /// Free space (GB) on the volume that holds `~/Library/Application
    /// Support/dimmy`. Returns nil if the OS won't tell us.
    private func availableDiskGB() -> Double? {
        guard let url = try? FileManager.default.url(
            for: .applicationSupportDirectory,
            in: .userDomainMask,
            appropriateFor: nil,
            create: false)
        else { return nil }
        let values = try? url.resourceValues(forKeys: [.volumeAvailableCapacityForImportantUsageKey])
        guard let capacityBytes = values?.volumeAvailableCapacityForImportantUsage else {
            return nil
        }
        return Double(capacityBytes) / (1024.0 * 1024.0 * 1024.0)
    }

    private var isParakeet: Bool { selection == Self.parakeetTag }

    private var currentDescription: String {
        if isParakeet {
            return "NVIDIA Parakeet on Apple Neural Engine — fastest local STT on Apple Silicon (~50× realtime). Italian quality matches cloud Groq."
        }
        switch selection {
        case "ggml-tiny-q8_0.bin": return "Whisper Tiny — fastest, lower accuracy."
        case "ggml-base-q8_0.bin": return "Whisper Base — good balance of speed and accuracy."
        case "ggml-small-q8_0.bin": return "Whisper Small — high accuracy, slower."
        case "ggml-medium-q8_0.bin": return "Whisper Medium — very high accuracy, needs 2 GB+ RAM."
        default: return ""
        }
    }

    private var downloadButtonLabel: String {
        isParakeet ? "Download Parakeet (466 MB)" : "Download model"
    }

    private var currentProgress: Double {
        isParakeet ? appState.parakeetDownloadProgress : appState.modelDownloadProgress
    }

    private func refreshFromCore() {
        let ready: Bool
        if isParakeet {
            ready = DimmyCore.shared.parakeetBundlePresent()
        } else {
            ready = DimmyCore.shared.modelExists(selection)
        }
        downloadState = ready ? .completed : .notStarted
    }

    private func startDownload() {
        downloadState = .downloading
        if isParakeet {
            appState.localSttBackend = "parakeet"
            appState.parakeetDownloadProgress = 0.0
            appState.isDownloadingParakeet = true
            DispatchQueue.global(qos: .userInitiated).async {
                let success = DimmyCore.shared.downloadParakeetBundle()
                DispatchQueue.main.async {
                    appState.isDownloadingParakeet = false
                    appState.parakeetBundlePresent = success
                    downloadState = success ? .completed : .notStarted
                }
            }
        } else {
            appState.localSttBackend = "whisper"
            appState.localModel = selection
            appState.isDownloadingModel = true
            appState.modelDownloadProgress = 0.0
            let target = selection
            DispatchQueue.global(qos: .userInitiated).async {
                let success = DimmyCore.shared.downloadModel(target)
                DispatchQueue.main.async {
                    appState.isDownloadingModel = false
                    downloadState = success ? .completed : .notStarted
                }
            }
        }
    }
}

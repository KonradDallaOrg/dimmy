import SwiftUI

struct ModelDownloadStepView: View {
    @ObservedObject var appState: AppState

    @State private var downloadState: DownloadState = .notStarted

    /// Sentinel for the Parakeet entry. Same value used by the Settings
    /// page + the Windows onboarding (`ParakeetTag` constant).
    private static let parakeetTag = "parakeet:fp32"
    private static let defaultWhisper = "ggml-base-q8_0.bin"

    /// What the user picks here lands in appState.localSttBackend +
    /// appState.localModel. Default is Parakeet on Apple Silicon — fastest
    /// local STT via Apple Neural Engine. `applyAutoPick` downgrades to
    /// Whisper Base only when the disk is too small for the 466 MB bundle.
    @State private var selection: String = parakeetTag

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
        }
        .padding(.horizontal, 40)
        .onAppear {
            persistSelectionToAppState()
            refreshFromCore()
            applyAutoPick()
        }
        .onChange(of: selection) {
            // Mirror the picker choice into AppState so Settings → Voice
            // reflects whatever the user picked here, even without ever
            // triggering a download.
            persistSelectionToAppState()
            // If the new selection is already on disk, jump straight
            // to .completed — the user can Continue without re-downloading.
            refreshFromCore()
        }
    }

    /// Smart default: on Apple Silicon with >= 2 GB free disk Parakeet is
    /// already the initial @State pick + AppDelegate has been preloading
    /// the bundle since launch. Here we just kick the foreground download
    /// (idempotent — the FFI dedup short-circuits when the preload is
    /// already in flight). With less disk we downgrade to Whisper Base.
    /// On < 200 MB we leave the picker on Parakeet but skip the auto-
    /// download — the user must free space and click Download manually.
    private func applyAutoPick() {
        let freeGB = availableDiskGB() ?? 0
        if freeGB < 2 {
            // Downgrade to whisper-base; the .onChange listener will
            // refresh state (probably .completed if it's already on disk
            // from a previous install).
            if selection != Self.defaultWhisper {
                selection = Self.defaultWhisper
            }
            return
        }
        // Disk OK → Parakeet stays. Trigger startDownload only if a
        // download isn't already in progress and the bundle isn't on
        // disk. startDownload() has its own dedup against the
        // AppDelegate-side preload.
        DispatchQueue.main.async {
            if downloadState == .notStarted {
                startDownload()
            }
        }
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

    /// Mirror the local `selection` into AppState AND push to Rust so the
    /// choice survives both onboarding and the next launch (Rust config is
    /// the source of truth on reload). Without the explicit setConfig
    /// call, the user picks Parakeet here, hits Continue, and Settings →
    /// Voice shows `Whisper Base` again on next launch.
    private func persistSelectionToAppState() {
        if isParakeet {
            appState.localSttBackend = "parakeet"
        } else {
            appState.localSttBackend = "whisper"
            appState.localModel = selection
        }
        DimmyCore.shared.setConfig(appState.toRustConfig())
    }

    private func refreshFromCore() {
        let ready: Bool
        if isParakeet {
            ready = DimmyCore.shared.parakeetBundlePresent()
        } else {
            ready = DimmyCore.shared.modelExists(selection)
        }
        // Honour an in-flight background preload: AppDelegate kicks off
        // the Parakeet download at boot when eligibility passes, so by
        // the time the user lands on this step the file may already be
        // streaming. Show progress instead of asking the user to start
        // a second download.
        if isParakeet && !ready && appState.isDownloadingParakeet {
            downloadState = .downloading
            return
        }
        if !isParakeet && !ready && appState.isDownloadingModel {
            downloadState = .downloading
            return
        }
        downloadState = ready ? .completed : .notStarted
    }

    private func startDownload() {
        // Don't fork a second download if the AppDelegate-side preload is
        // already pumping bytes into the same on-disk dir. Without this
        // guard the FFI gets two concurrent download_active_bundle calls
        // racing on the same files.
        if isParakeet && appState.isDownloadingParakeet {
            downloadState = .downloading
            return
        }
        if !isParakeet && appState.isDownloadingModel {
            downloadState = .downloading
            return
        }
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

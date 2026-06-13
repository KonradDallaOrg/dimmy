import SwiftUI

struct ModelDownloadStepView: View {
    @ObservedObject var appState: AppState

    @State private var downloadState: DownloadState = .notStarted

    /// Sentinel for the Parakeet entry. Same value used by the Settings
    /// page + the Windows onboarding (`ParakeetTag` constant).
    private static let parakeetTag = "parakeet:fp32"
    private static let defaultWhisper = "ggml-base-q8_0.bin"

    /// Whisper catalog from the Rust core (`dimmy_list_local_models`) so
    /// onboarding offers the same models as Settings + Windows, not a
    /// hardcoded subset of three. Parakeet is prepended separately (it's
    /// the recommended default, not part of the whisper catalog).
    @State private var whisperModels: [[String: Any]] = []

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
        case failed
    }

    /// Shown under the "Download failed" header. Generic copy: the FFI
    /// only reports success/failure, and the detailed cause is in the
    /// core log either way.
    @State private var failureText = "Check your internet connection and try again."

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
                    Text("Parakeet TDT v3 · 466 MB · Apple Neural Engine (recommended)").tag(Self.parakeetTag)
                    ForEach(whisperModels.indices, id: \.self) { i in
                        Text(Self.whisperLabel(whisperModels[i]))
                            .tag(whisperModels[i]["filename"] as? String ?? "")
                    }
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

            case .failed:
                VStack(spacing: 8) {
                    HStack(spacing: 6) {
                        Image(systemName: "exclamationmark.triangle.fill")
                            .foregroundColor(.orange)
                        Text("Download failed")
                    }
                    .font(.headline)
                    Text(failureText)
                        .font(.caption)
                        .foregroundColor(.secondary)
                        .multilineTextAlignment(.center)
                    Button("Try again") { startDownload() }
                        .buttonStyle(.borderedProminent)
                        .controlSize(.large)
                }
            }

            Spacer()
        }
        .padding(.horizontal, 40)
        .onAppear {
            whisperModels = DimmyCore.shared.listLocalModels() ?? []
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
        guard let m = whisperModels.first(where: { ($0["filename"] as? String) == selection }) else {
            return ""
        }
        let name = m["name"] as? String ?? "Whisper"
        let desc = m["description"] as? String ?? ""
        return desc.isEmpty ? "Whisper \(name)" : "Whisper \(name) — \(desc)."
    }

    /// Picker label for a whisper model dict: "Whisper Large-v3-Turbo Q8 · 874 MB".
    private static func whisperLabel(_ m: [String: Any]) -> String {
        let name = (m["name"] as? String) ?? (m["filename"] as? String) ?? "Model"
        let mb = m["size_mb"] as? Int ?? 0
        let size: String
        if mb >= 1024 {
            size = String(format: "%.1f GB", Double(mb) / 1024.0)
        } else if mb > 0 {
            size = "\(mb) MB"
        } else {
            size = ""
        }
        return size.isEmpty ? "Whisper \(name)" : "Whisper \(name) · \(size)"
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
                    downloadState = success ? .completed : .failed
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
                    downloadState = success ? .completed : .failed
                }
            }
        }
    }
}

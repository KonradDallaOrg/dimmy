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

    /// Two-card mode selector. Mirrors Windows `IsLocalSelected` /
    /// `IsCloudSelected`. Defaults to local on Mac (no key required to
    /// finish the wizard offline).
    @State private var pickedMode: PickedMode = .local

    @State private var groqKey: String = ""
    @State private var cloudState: CloudState = .empty
    @State private var cloudError: String = ""
    @State private var validateTask: Task<Void, Never>?

    enum DownloadState {
        case notStarted
        case downloading
        case completed
        case skipped
        case failed
    }

    enum PickedMode { case local, cloud }

    enum CloudState { case empty, validating, ready, failed }

    /// Shown under the "Download failed" header. Generic copy: the FFI
    /// only reports success/failure, and the detailed cause is in the
    /// core log either way.
    @State private var failureText = "Check your internet connection and try again."

    var body: some View {
        VStack(spacing: 12) {
            Text("How should Dimmy transcribe?")
                .font(.title2.bold())
            Text("Private and offline, or fastest with cloud")
                .font(.system(size: 12))
                .foregroundColor(.secondary)

            HStack(alignment: .top, spacing: 12) {
                localCard
                cloudCard
            }
            .padding(.top, 4)

            Spacer(minLength: 0)
        }
        .padding(.horizontal, 24)
        .padding(.top, 12)
        .onAppear {
            whisperModels = DimmyCore.shared.listLocalModels() ?? []
            persistSelectionToAppState()
            refreshFromCore()
            applyAutoPick()
            // The card highlight reflects what's actually configured.
            // A pre-existing Groq key (Run setup again, or pasted in
            // Settings before) flips the Cloud card to "Ready" even if
            // the user's current sttMode is still local — they can see
            // the key is there without re-typing.
            pickedMode = appState.sttMode == "cloud" ? .cloud : .local
            if DimmyCore.shared.hasApiKey {
                cloudState = .ready
            }
        }
        .onChange(of: selection) {
            persistSelectionToAppState()
            refreshFromCore()
        }
    }

    // MARK: Local card

    private var localCard: some View {
        cardContainer(selected: pickedMode == .local, onTap: selectLocal) {
            VStack(alignment: .leading, spacing: 8) {
                Image(systemName: "cpu")
                    .font(.system(size: 20))
                    .foregroundColor(.accentColor)
                Text("Local")
                    .font(.system(size: 15, weight: .semibold))
                Text("Private · offline · runs on your Mac")
                    .font(.system(size: 11))
                    .foregroundColor(.secondary)

                Picker("", selection: $selection) {
                    parakeetPickerEntry
                    ForEach(whisperModels.indices, id: \.self) { i in
                        whisperPickerEntry(whisperModels[i])
                    }
                }
                .labelsHidden()
                .pickerStyle(.menu)
                .frame(maxWidth: .infinity, alignment: .leading)
                .disabled(downloadState == .downloading)

                switch downloadState {
                case .notStarted:
                    Button(downloadButtonLabel) { startDownload() }
                        .buttonStyle(.bordered)
                        .controlSize(.small)
                case .downloading:
                    VStack(alignment: .leading, spacing: 4) {
                        ProgressView(value: currentProgress, total: 1.0)
                        Text("\(Int(currentProgress * 100))%")
                            .font(.system(size: 10))
                            .foregroundColor(.secondary)
                    }
                case .completed:
                    HStack(spacing: 4) {
                        Image(systemName: "checkmark.circle.fill")
                            .foregroundColor(.green)
                        Text("Ready to use")
                            .font(.system(size: 11))
                            .foregroundColor(.secondary)
                    }
                case .skipped:
                    Text("Download later in Settings.")
                        .font(.system(size: 10))
                        .foregroundColor(.secondary)
                case .failed:
                    VStack(alignment: .leading, spacing: 4) {
                        HStack(spacing: 4) {
                            Image(systemName: "exclamationmark.triangle.fill")
                                .foregroundColor(.orange)
                            Text("Download failed")
                                .font(.system(size: 11))
                        }
                        Text(failureText)
                            .font(.system(size: 10))
                            .foregroundColor(.secondary)
                        Button("Try again") { startDownload() }
                            .buttonStyle(.bordered)
                            .controlSize(.small)
                    }
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    // MARK: Cloud card

    private var cloudCard: some View {
        cardContainer(selected: pickedMode == .cloud, onTap: selectCloud) {
            VStack(alignment: .leading, spacing: 8) {
                Image(systemName: "cloud.fill")
                    .font(.system(size: 20))
                    .foregroundColor(.accentColor)
                Text("Cloud (Groq)")
                    .font(.system(size: 15, weight: .semibold))
                Text("Fastest · most accurate")
                    .font(.system(size: 11))
                    .foregroundColor(.secondary)

                SecureField("gsk_...", text: $groqKey)
                    .textFieldStyle(.roundedBorder)
                    .controlSize(.small)
                    .onChange(of: groqKey) {
                        cloudState = .empty
                        cloudError = ""
                        scheduleValidate()
                    }

                Link(destination: URL(string: "https://console.groq.com/keys")!) {
                    Text("Get API key \u{2192}")
                        .font(.system(size: 11))
                }

                cloudStatusRow
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    @ViewBuilder
    private var cloudStatusRow: some View {
        switch cloudState {
        case .empty:
            EmptyView()
        case .validating:
            HStack(spacing: 4) {
                ProgressView().controlSize(.mini)
                Text("Validating…")
                    .font(.system(size: 10))
                    .foregroundColor(.secondary)
            }
        case .ready:
            HStack(spacing: 4) {
                Image(systemName: "checkmark.circle.fill")
                    .foregroundColor(.green)
                Text("Ready to use")
                    .font(.system(size: 11))
                    .foregroundColor(.secondary)
            }
        case .failed:
            Text(cloudError)
                .font(.system(size: 10))
                .foregroundColor(.red)
                .lineLimit(2)
        }
    }

    // MARK: Card container

    @ViewBuilder
    private func cardContainer<Content: View>(
        selected: Bool,
        onTap: @escaping () -> Void,
        @ViewBuilder content: () -> Content
    ) -> some View {
        content()
            .padding(12)
            .frame(maxWidth: .infinity, minHeight: 240, alignment: .topLeading)
            .background(
                RoundedRectangle(cornerRadius: 10, style: .continuous)
                    .fill(Color(nsColor: .windowBackgroundColor).opacity(0.6))
            )
            .overlay(
                RoundedRectangle(cornerRadius: 10, style: .continuous)
                    .stroke(selected ? Color.accentColor : Color.macStrokeHairline,
                            lineWidth: selected ? 2 : 0.5)
            )
            .contentShape(Rectangle())
            .onTapGesture(perform: onTap)
    }

    // MARK: Cloud validation

    private func scheduleValidate() {
        validateTask?.cancel()
        let key = groqKey
        guard !key.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return }
        cloudState = .validating
        validateTask = Task { @MainActor in
            try? await Task.sleep(nanoseconds: 350_000_000)  // 350 ms debounce
            if Task.isCancelled { return }
            let result = await GroqKeyValidator.validate(key)
            if Task.isCancelled { return }
            switch result {
            case .ok:
                cloudState = .ready
                cloudError = ""
                persistCloudKey(key)
                pickedMode = .cloud
            case .invalid(let reason):
                cloudState = .failed
                cloudError = reason
            }
        }
    }

    /// Push the validated Groq key + cloud STT config to the Rust core.
    /// `api_key` rides on the JSON envelope; the Rust side moves it to
    /// keys.enc and strips it before persisting config.json.
    ///
    /// Reload the config back after the write — otherwise
    /// `appState.hasGroqKey` stays at its pre-write value and the next
    /// step's `needsCloudKey = sttMode=="cloud" && !hasKey` fires a
    /// phantom "Add API key in Settings" panel right after the user
    /// pasted a valid key. Mirror of `MacVoicePage.saveApiKey`.
    private func persistCloudKey(_ key: String) {
        appState.sttMode = "cloud"
        appState.apiUrl = "https://api.groq.com/openai/v1/audio/transcriptions"
        appState.apiModel = "whisper-large-v3-turbo"
        var config = appState.toRustConfig()
        config["api_key"] = key.trimmingCharacters(in: .whitespacesAndNewlines)
        DimmyCore.shared.setConfig(config)
        if let cfg = DimmyCore.shared.getConfig() {
            appState.loadFromRustConfig(cfg)
        }
    }

    /// Tapping a card commits the choice to `sttMode` — without this,
    /// the highlight flipped visually but the wizard exited with the
    /// other mode still persisted in config. Local + cloud key both
    /// configured? Tap Cloud → sttMode flips back to "cloud", no
    /// re-type required.
    private func selectLocal() {
        pickedMode = .local
        guard appState.sttMode != "local" else { return }
        appState.sttMode = "local"
        DimmyCore.shared.setConfig(appState.toRustConfig())
        if let cfg = DimmyCore.shared.getConfig() {
            appState.loadFromRustConfig(cfg)
        }
    }

    private func selectCloud() {
        pickedMode = .cloud
        guard appState.sttMode != "cloud" else { return }
        appState.sttMode = "cloud"
        DimmyCore.shared.setConfig(appState.toRustConfig())
        if let cfg = DimmyCore.shared.getConfig() {
            appState.loadFromRustConfig(cfg)
        }
    }

    // MARK: Local logic (unchanged from prior version)

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
            if selection != Self.defaultWhisper {
                selection = Self.defaultWhisper
            }
            return
        }
        DispatchQueue.main.async {
            if downloadState == .notStarted {
                startDownload()
            }
        }
    }

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

    /// Picker rows. Downloaded entries (whisper via `downloaded` from
    /// `dimmy_list_local_models`, Parakeet via `parakeetBundlePresent`)
    /// get a green ✓; not-downloaded rows stay plain. Mirror of the
    /// Settings → Voice picker so the visual contract matches.
    @ViewBuilder
    private var parakeetPickerEntry: some View {
        if appState.parakeetBundlePresent {
            Label("Parakeet TDT v3", systemImage: "checkmark.circle.fill")
                .foregroundStyle(.green)
                .tag(Self.parakeetTag)
        } else {
            Text("Parakeet TDT v3").tag(Self.parakeetTag)
        }
    }

    @ViewBuilder
    private func whisperPickerEntry(_ m: [String: Any]) -> some View {
        let filename = m["filename"] as? String ?? ""
        let label = Self.whisperLabel(m)
        if (m["downloaded"] as? Bool) == true {
            Label(label, systemImage: "checkmark.circle.fill")
                .foregroundStyle(.green)
                .tag(filename)
        } else {
            Text(label).tag(filename)
        }
    }

    /// Picker label for a whisper model dict: "Whisper Base · 142 MB".
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

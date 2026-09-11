import SwiftUI

// Voice input, STT mode + provider + API key + language + microphone +
// audio processing. Consolidates the legacy `General` and `Models` tabs
// into a single page matching the design handoff `MacVoice` component.

struct MacVoicePage: View {
    @ObservedObject var appState: AppState

    @State private var apiKeyInput: String = ""
    @State private var showKeyField: Bool = false

    @State private var localModelExists: Bool = false
    @State private var downloadInFlight: Bool = false
    // What the running download is, captured when it STARTS. Reading the
    // picker live meant switching models mid-download renamed the bar without
    // changing what it measured.
    @State private var downloadingTarget: String = ""
    @State private var downloadingLabel: String = ""
    @State private var downloadingIsQwen: Bool = false
    @State private var downloadingIsParakeet: Bool = false
    @State private var downloadFailed: String? = nil
    /// whisper's Core ML encoder for the selected model. Without it the
    /// encoder runs on the GPU the window server draws with, which is what
    /// makes a long local meeting slow the whole Mac down.
    @State private var coreml: (available: Bool, present: Bool) = (false, false)

    /// Whisper model catalog, loaded from the Rust core's single source
    /// of truth (`dimmy_list_local_models`) so the Mac picker offers the
    /// SAME set as Windows, incl. the turbo / large-v3 / distil-EN
    /// variants. Previously this Picker hardcoded only 4 entries, so Mac
    /// users never saw the larger/faster models the core already supports.
    @State private var localModels: [[String: Any]] = []
    @State private var qwenModels: [[String: Any]] = []

    /// Text-field state for the "add a word" row in the custom-dictionary
    /// section. Kept inline to avoid a parallel view-model class, the
    /// list itself lives on AppState and `addDictWord` calls the FFI so
    /// the Rust core remains the single writer.
    @State private var newDictWord: String = ""
    @State private var dictAddError: String? = nil

    /// Sentinel value for the Parakeet entry in the unified local-model
    /// Picker. Mirrors `ParakeetTag` in the Windows OnboardingWindow.xaml.cs
    /// so the two UIs round-trip the same selection through the Rust core.
    private static let parakeetTag = "parakeet:fp32"

    /// Qwen3-ASR rows carry the variant in the tag, because unlike
    /// Parakeet there is more than one of them. Mirrors QwenTagPrefix in
    /// SettingsWindow.xaml.cs so both UIs round-trip the same selection.
    private static let qwenTagPrefix = "qwen:"

    /// Picker label for one whisper model dict from `listLocalModels()`:
    /// "Large-v3-Turbo Q8 · 874 MB". The on-disk state is rendered as a
    /// green ✓ next to the row (see `whisperPickerItem` below), not in
    /// the text.
    private static func modelLabel(_ m: [String: Any]) -> String {
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
        return size.isEmpty ? name : "\(name) · \(size)"
    }

    /// One row inside the local-model Picker. Downloaded entries get a
    /// green check (`checkmark.circle.fill`); not-yet-downloaded rows
    /// stay plain text so the visual delta is unambiguous. The Rust
    /// core's `dimmy_list_local_models` already ships a `downloaded:
    /// bool` per entry (see `core/src/ffi.rs::dimmy_list_local_models`)
    /// so Windows can do the exact same with no FFI change.
    @ViewBuilder
    fileprivate static func whisperPickerItem(_ m: [String: Any]) -> some View {
        let filename = m["filename"] as? String ?? ""
        let label = modelLabel(m)
        if (m["downloaded"] as? Bool) == true {
            Label(label, systemImage: "checkmark.circle.fill")
                .foregroundStyle(.green)
                .tag(filename)
        } else {
            Text(label).tag(filename)
        }
    }

    /// Parakeet picker row. The Mac path checks the in-process bundle
    /// flag instead of the listLocalModels output because Parakeet is
    /// not a whisper file — it's the CoreML/Fluid bundle owned by
    /// `parakeet_fluid.rs` and surfaced via `parakeetBundlePresent`.
    /// One Qwen3-ASR row. Presence is the pair check, not a single
    /// file, so it comes from `qwenAsrBundlePresent` rather than the
    /// whisper listing.
    @ViewBuilder
    fileprivate static func qwenPickerItem(_ m: [String: Any]) -> some View {
        let file = m["filename"] as? String ?? ""
        let label = modelLabel(m)
        if (m["downloaded"] as? Bool) == true {
            Label(label, systemImage: "checkmark.circle.fill")
                .foregroundStyle(.green)
                .tag(qwenTagPrefix + file)
        } else {
            Text(label).tag(qwenTagPrefix + file)
        }
    }

    @ViewBuilder
    fileprivate static func parakeetPickerItem(
        label: String,
        present: Bool
    ) -> some View {
        if present {
            Label(label, systemImage: "checkmark.circle.fill")
                .foregroundStyle(.green)
                .tag(parakeetTag)
        } else {
            Text(label).tag(parakeetTag)
        }
    }

    /// User-facing label for the call-detect exclusion list. Maps the
    /// canonical lowercase id ("teams", "zoom", ...) back to the brand
    /// name. Mirror of CallNudgeWindowController.appDisplayNames.
    private static func exclusionDisplayName(for app: String) -> String {
        switch app.lowercased() {
        case "teams": return "Microsoft Teams"
        case "zoom": return "Zoom"
        case "slack": return "Slack"
        case "discord": return "Discord"
        case "webex": return "Cisco Webex"
        default: return app.capitalized
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            speechRecognitionGroup

            // Per settings-map.md "Vuoi" column: Microphone (mic gain
            // + Preprocessing + Chunk streaming + Live captions),
            // Custom dictionary and the Vocabulary (Recognition
            // prompt) section all live behind Advanced. Simple view
            // shows only Speech recognition.
            if appState.showAdvanced {
                microphoneGroup
                audioProcessingGroup
                customDictionaryGroup
                advancedGroup
            }
        }
    }

    // MARK: Custom dictionary
    //
    // Wispr Flow-style user-curated vocabulary. Surfaces at top level
    // (not gated by Advanced) because the feature is the user's main
    // entry point to teach Dimmy domain words. Each word is sent through
    // the FFI; the Rust core dedupes case-insensitively and persists.

    private var customDictionaryGroup: some View {
        Group {
            MacGroupLabel(text: "Custom dictionary (\(appState.userDictWords.count))")
            MacTile {
                VStack(alignment: .leading, spacing: 10) {
                    HStack(spacing: 8) {
                        TextField("Add a word or short phrase", text: $newDictWord)
                            .textFieldStyle(.roundedBorder)
                            .onSubmit { addDictWord() }
                        Button("Add") { addDictWord() }
                            .disabled(newDictWord.trimmingCharacters(in: .whitespaces).isEmpty)
                            .keyboardShortcut(.defaultAction)
                    }
                    if let err = dictAddError {
                        Text(err)
                            .font(.system(size: 11))
                            .foregroundStyle(.orange)
                    }

                    // Single-line workflow hint. Toast handles the
                    // detail (showing the actual workflow on mistake);
                    // Settings just confirms which mode is active.
                    HStack(alignment: .center, spacing: 6) {
                        Image(systemName: PermissionsManager.shared.accessibilityGranted
                              ? "keyboard.fill"
                              : "exclamationmark.bubble.fill")
                            .font(.system(size: 11))
                            .foregroundStyle(PermissionsManager.shared.accessibilityGranted ? .green : .orange)
                        Text(PermissionsManager.shared.accessibilityGranted
                             ? "Hotkey ready: select text, press \(appState.dictHotkey.displayString)."
                             : "Hotkey needs Cmd+C first (Accessibility not granted).")
                            .font(.system(size: 11))
                            .foregroundStyle(Color.macTextSecondary)
                    }

                    if appState.userDictWords.isEmpty {
                        Text("No custom words yet.")
                            .font(.system(size: 11))
                            .foregroundStyle(Color.macTextSecondary)
                            .fixedSize(horizontal: false, vertical: true)
                    } else {
                        ScrollView {
                            VStack(alignment: .leading, spacing: 4) {
                                ForEach(appState.userDictWords, id: \.self) { word in
                                    HStack {
                                        Text(word)
                                            .font(.system(size: 12))
                                        Spacer()
                                        Button {
                                            removeDictWord(word)
                                        } label: {
                                            Image(systemName: "minus.circle.fill")
                                                .foregroundStyle(.secondary)
                                        }
                                        .buttonStyle(.plain)
                                        .help("Remove \(word)")
                                    }
                                    .padding(.horizontal, 10)
                                    .padding(.vertical, 6)
                                    .background(
                                        RoundedRectangle(cornerRadius: 6, style: .continuous)
                                            .fill(Color.primary.opacity(0.04))
                                    )
                                }
                            }
                        }
                        .frame(maxHeight: 180)
                    }
                }
                .padding(EdgeInsets(top: 12, leading: 14, bottom: 12, trailing: 14))
            }
            MacGroupFooter(text: "Words listed here bias the STT engine. Parakeet ignores them, its API has no boost-word slot.")
        }
    }

    private func addDictWord() {
        let trimmed = newDictWord.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        if trimmed.count > 100 {
            dictAddError = "Word too long (max 100 chars)"
            return
        }
        let result = DimmyCore.shared.userDictAdd(trimmed)
        switch result {
        case .added:
            if !appState.userDictWords.contains(where: { $0.lowercased() == trimmed.lowercased() }) {
                appState.userDictWords.append(trimmed)
            }
            newDictWord = ""
            dictAddError = nil
        case .alreadyPresent:
            dictAddError = "'\(trimmed)' is already in the dictionary"
            newDictWord = ""
        case .error:
            dictAddError = "Could not add, check log"
        }
    }

    private func removeDictWord(_ word: String) {
        let count = DimmyCore.shared.userDictRemove(word)
        if count >= 0 {
            appState.userDictWords.removeAll { $0.lowercased() == word.lowercased() }
        }
    }

    // MARK: Speech recognition

    private var speechRecognitionGroup: some View {
        Group {
            MacGroupLabel(text: "Speech recognition")
            MacTile {
                MacRow(
                    "Mode",
                    hint: "Local is fully private and works offline. Cloud sends your audio to the provider you pick and is often more accurate.",
                    hintURL: URL(string: "https://dimmy.app/help/local-mode")
                ) {
                    Picker("", selection: Binding(
                        get: { appState.sttMode },
                        set: { newValue in
                            appState.sttMode = newValue
                            persistConfig()
                        }
                    )) {
                        Text("On device").tag("local")
                        Text("Cloud").tag("cloud")
                    }
                    .pickerStyle(.segmented)
                    .labelsHidden()
                    .frame(width: 180)
                }

                if appState.sttMode == "cloud" {
                    let availablePresets = appState.availableSttPresets()
                    let hasOnlyCustom = availablePresets.count == 1 && availablePresets.first?.provider == .custom
                    MacRow(
                        "Provider",
                        hint: "Only providers you've connected on Providers and keys are shown. Pick the model that suits your language and speed needs.",
                        hintURL: URL(string: "https://dimmy.app/help/cloud-providers")
                    ) {
                        Picker("", selection: sttPresetBinding) {
                            ForEach(availablePresets) { preset in
                                Label {
                                    Text(preset.displayName)
                                } icon: {
                                    if preset.iconAssetName.isEmpty {
                                        Image(systemName: "gear")
                                    } else {
                                        Image(preset.iconAssetName)
                                            .renderingMode(.original)
                                            .resizable()
                                            .scaledToFit()
                                            .frame(width: 18, height: 18)
                                    }
                                }
                                .tag(preset.id)
                            }
                        }
                        .labelsHidden()
                        .frame(width: 320)
                    }

                    if hasOnlyCustom {
                        MacRow(
                            "",
                            description: "No cloud STT providers connected yet.",
                            showsDivider: true
                        ) {
                            Button("Open Providers and keys") {
                                NotificationCenter.default.post(
                                    name: .dimmyNavigateSettingsTab,
                                    object: nil,
                                    userInfo: ["tab": "providers"]
                                )
                            }
                            .controlSize(.small)
                        }
                    }

                    // Custom keeps the inline key field (it's the only
                    // STT preset whose URL is also user-supplied — the
                    // Providers and keys page has no Custom card).
                    // Every other provider routes through the Providers
                    // and keys page: one source of truth, single FFI
                    // refresh on save. Mirror of Win
                    // ProviderCatalog.IsKeyableHere == false for custom.
                    let currentPresetIsCustom = appState.apiUrl.isEmpty
                    if currentPresetIsCustom {
                        MacRow(
                            "API key",
                            description: "Encrypted locally.",
                            hint: "Stored encrypted on this device with AES-256. The provider only ever receives your audio and this key, nothing else.",
                            hintURL: URL(string: "https://dimmy.app/help/api-keys"),
                            showsDivider: showKeyField
                        ) {
                            if appState.hasKey {
                                HStack(spacing: 4) {
                                    Image(systemName: "checkmark.circle.fill")
                                        .foregroundStyle(.green)
                                    Text("Saved")
                                        .font(.system(size: 12, weight: .medium))
                                        .foregroundStyle(.green)
                                }
                            }
                            Button(appState.hasKey ? (showKeyField ? "Cancel" : "Replace...")
                                                   : (showKeyField ? "Cancel" : "Add key...")) {
                                showKeyField.toggle()
                                if !showKeyField { apiKeyInput = "" }
                            }
                            .controlSize(.small)
                        }
                        if showKeyField {
                            apiKeyEntryRow
                        }
                    } else {
                        MacRow(
                            "API key",
                            description: appState.hasKey
                                ? "Saved. Managed in Providers and keys."
                                : "Not connected. Set it in Providers and keys.",
                            hint: "Keys live in one place: Providers and keys. Connect once there, every page picks it up.",
                            hintURL: URL(string: "https://dimmy.app/help/api-keys"),
                            showsDivider: false
                        ) {
                            if appState.hasKey {
                                HStack(spacing: 4) {
                                    Image(systemName: "checkmark.circle.fill")
                                        .foregroundStyle(.green)
                                    Text("Saved")
                                        .font(.system(size: 12, weight: .medium))
                                        .foregroundStyle(.green)
                                }
                            }
                            Button("Manage on Providers and keys") {
                                NotificationCenter.default.post(
                                    name: .dimmyNavigateSettingsTab,
                                    object: nil,
                                    userInfo: ["tab": "providers"]
                                )
                            }
                            .controlSize(.small)
                        }
                    }
                } else {
                    MacRow(
                        "Local model",
                        hint: "Whisper sizes run fully offline. Parakeet TDT v3 is faster and strong on European languages, and downloads once at about 466 MB.",
                        hintURL: URL(string: "https://dimmy.app/help/whisper-models"),
                        showsDivider: !localModelReady || downloadInFlight || coremlRowVisible
                    ) {
                        Picker("", selection: localModelPickerBinding) {
                            ForEach(localModels.indices, id: \.self) { i in
                                Self.whisperPickerItem(localModels[i])
                            }
                            Self.parakeetPickerItem(
                                label: "Parakeet TDT v3 · 466 MB · Apple Neural Engine",
                                present: appState.parakeetBundlePresent
                            )
                            ForEach(qwenModels.indices, id: \.self) { i in
                                Self.qwenPickerItem(qwenModels[i])
                            }
                        }
                        .labelsHidden()
                        .frame(width: 260)
                    }

                    if downloadInFlight {
                        // Both the number and the name come from the download
                        // that is actually running, captured when it started.
                        // They used to be read live from the picker, so
                        // switching models mid-download left the bar showing
                        // one file under the other one's name.
                        modelProgressRow(
                            progress: downloadingProgress,
                            label: "Downloading \(downloadingLabel)..."
                        )
                    } else if !localModelReady {
                        MacRow(
                            "Download",
                            description: downloadFailed ?? (localBackendIsParakeet
                                ? "Parakeet CoreML bundle (about 466 MB) isn't on disk yet."
                                : "This model isn't on disk yet."),
                            showsDivider: false
                        ) {
                            Button(localBackendIsParakeet
                                   ? "Download bundle" : "Download model") {
                                startSttDownload()
                            }
                            .buttonStyle(.borderedProminent)
                            .controlSize(.small)
                        }
                    } else if coremlRowVisible {
                        MacRow(
                            "Neural Engine",
                            description: coreml.present
                                ? "On. whisper's encoder runs on the Neural Engine, leaving the GPU to the rest of the Mac."
                                : "Moves whisper's encoder off the GPU, so a long meeting doesn't slow the whole Mac. About 1.2 GB for large models; the first transcription afterwards takes a few minutes while macOS compiles it.",
                            showsDivider: false
                        ) {
                            if coreml.present {
                                Image(systemName: "checkmark.circle.fill")
                                    .foregroundStyle(.green)
                            } else {
                                Button("Download") { startCoremlDownload() }
                                    .buttonStyle(.bordered)
                                    .controlSize(.small)
                            }
                        }
                    }
                }

                MacRow(
                    "Language",
                    hint: "Tells the speech engine what to expect. Auto-detect works with cloud and local models; picking the language you speak is still a little faster and more reliable on short clips. To translate into another language, use the pill's scroll wheel instead.",
                    hintURL: URL(string: "https://dimmy.app/help/language")
                ) {
                    Picker("", selection: Binding(
                        get: { appState.selectedLanguage },
                        set: { newValue in
                            appState.selectedLanguage = newValue
                            persistConfig()
                        }
                    )) {
                        ForEach(appState.languages, id: \.self) { lang in
                            Text(lang).tag(lang)
                        }
                    }
                    .labelsHidden()
                    .frame(width: 160)
                }

                // Input device PROMOTED to Simple per
                // settings-redesign-checklist.md ("was wrongly under
                // Advanced"). Microphone gain / preprocessing / chunk
                // / live-captions stay behind Advanced.
                MacRow(
                    "Input device",
                    hint: "System audio loopback (used for meetings) always records from your default playback device, whatever you pick here.",
                    hintURL: URL(string: "https://dimmy.app/help/audio-input"),
                    showsDivider: false
                ) {
                    if appState.devices.isEmpty {
                        Text("System default")
                            .font(.system(size: 12))
                            .foregroundStyle(Color.macTextSecondary)
                    } else {
                        Picker("", selection: Binding(
                            get: { appState.selectedDevice ?? "" },
                            set: { newValue in
                                appState.selectedDevice = newValue.isEmpty ? nil : newValue
                                persistConfig()
                            }
                        )) {
                            Text("System default").tag("")
                            ForEach(appState.devices, id: \.self) { dev in
                                Text(dev).tag(dev)
                            }
                        }
                        .labelsHidden()
                        .frame(width: 240)
                    }
                }
            }
        }
        .onAppear {
            localModels = DimmyCore.shared.listLocalModels() ?? []
            refreshLocalModelStatus()
        }
    }

    private var localBackendIsParakeet: Bool {
        appState.localSttBackend == "parakeet"
    }

    private var localBackendIsQwen: Bool {
        appState.localSttBackend == "qwen"
    }

    /// True when the currently-selected local backend has its data on
    /// disk and is ready to transcribe. Whisper: ggml file present.
    /// Parakeet: full CoreML bundle (about 466 MB) present.
    private var localModelReady: Bool {
        if localBackendIsQwen {
            return appState.qwenBundlePresent
        }
        if localBackendIsParakeet {
            return appState.parakeetBundlePresent
        }
        return localModelExists
    }

    /// Single Picker binding that drives BOTH `localModel` (whisper
    /// filename) and `localSttBackend` ("whisper" | "parakeet"). Picking
    /// the Parakeet sentinel flips the backend without overwriting the
    /// remembered ggml choice, so toggling back restores the previous
    /// whisper model, same UX as the Windows ComboBox unification.
    private var localModelPickerBinding: Binding<String> {
        Binding(
            get: {
                if localBackendIsQwen {
                    return Self.qwenTagPrefix + appState.qwenAsrModel
                }
                return localBackendIsParakeet ? Self.parakeetTag : appState.localModel
            },
            set: { newValue in
                if newValue.hasPrefix(Self.qwenTagPrefix) {
                    appState.localSttBackend = "qwen"
                    appState.qwenAsrModel = String(newValue.dropFirst(Self.qwenTagPrefix.count))
                    // Same convenience as the other two backends: the
                    // low-latency chunked path is the reason to run a
                    // local engine at all.
                    appState.chunkStreamingEnabled = true
                } else if newValue == Self.parakeetTag {
                    appState.localSttBackend = "parakeet"
                    // Auto-enable chunk streaming on Parakeet pick. Mirror
                    // of SettingsWindow.xaml.cs:LocalModel_SelectionChanged
                    // on Windows, chunk streaming is Parakeet-only at
                    // runtime and the low-latency live-caption experience
                    // is the whole reason users pick Parakeet over Whisper.
                    appState.chunkStreamingEnabled = true
                } else {
                    appState.localSttBackend = "whisper"
                    appState.localModel = newValue
                }
                persistConfig()
                refreshLocalModelStatus()
            }
        )
    }

    private func refreshLocalModelStatus() {
        guard DimmyCore.shared.isInitialized else {
            localModelExists = false
            appState.parakeetBundlePresent = false
            appState.qwenBundlePresent = false
            return
        }
        // Move the FFI probes off the main thread. Each call is a
        // stat() / directory walk in Rust; individually cheap (<10 ms)
        // but when this runs synchronously inside `.onAppear` the
        // first tab-switch into Voice blocks the main thread before
        // SwiftUI can render the page, visible as a "slow click"
        // on the sidebar item. Dispatching them async lets the
        // page paint immediately and the model status fills in a
        // few milliseconds later.
        let modelName = appState.localModel
        let qwenName = appState.qwenAsrModel
        DispatchQueue.global(qos: .userInitiated).async {
            let exists = DimmyCore.shared.modelExists(modelName)
            let parakeet = DimmyCore.shared.parakeetBundlePresent()
            let models = DimmyCore.shared.listLocalModels() ?? []
            let qwen = DimmyCore.shared.qwenAsrBundlePresent(qwenName)
            let qwenList = DimmyCore.shared.listQwenAsrModels() ?? []
            let coremlStatus = DimmyCore.shared.coremlEncoderStatus(modelName)
            DispatchQueue.main.async {
                self.localModelExists = exists
                self.coreml = coremlStatus
                self.appState.parakeetBundlePresent = parakeet
                self.appState.qwenBundlePresent = qwen
                if !qwenList.isEmpty { self.qwenModels = qwenList }
                self.downloadFailed = nil
                if !models.isEmpty { self.localModels = models }
            }
        }
    }

    private func startSttDownload() {
        guard !downloadInFlight, DimmyCore.shared.isInitialized else { return }
        downloadInFlight = true
        downloadFailed = nil
        downloadingIsQwen = localBackendIsQwen
        downloadingIsParakeet = localBackendIsParakeet
        if localBackendIsQwen {
            let target = appState.qwenAsrModel
            downloadingTarget = target
            downloadingLabel = "\(target) and its projector"
            appState.qwenDownloadProgress = 0
            appState.isDownloadingQwen = true
            DispatchQueue.global(qos: .userInitiated).async {
                let ok = DimmyCore.shared.downloadQwenAsr(target)
                DispatchQueue.main.async {
                    downloadInFlight = false
                    appState.isDownloadingQwen = false
                    if ok {
                        refreshLocalModelStatus()
                    } else {
                        downloadFailed = "Qwen3-ASR download failed. Check your connection and try again."
                    }
                }
            }
        } else if localBackendIsParakeet {
            downloadingTarget = "parakeet"
            downloadingLabel = "Parakeet CoreML bundle (about 466 MB)"
            appState.parakeetDownloadProgress = 0
            appState.isDownloadingParakeet = true
            DispatchQueue.global(qos: .userInitiated).async {
                let ok = DimmyCore.shared.downloadParakeetBundle()
                DispatchQueue.main.async {
                    downloadInFlight = false
                    appState.isDownloadingParakeet = false
                    if ok {
                        refreshLocalModelStatus()
                    } else {
                        downloadFailed = "Parakeet download failed. Check your connection and try again."
                    }
                }
            }
        } else {
            let target = appState.localModel
            downloadingTarget = target
            downloadingLabel = target
            appState.modelDownloadProgress = 0
            appState.modelDownloadFilename = ""
            DispatchQueue.global(qos: .userInitiated).async {
                let ok = DimmyCore.shared.downloadModel(target)
                DispatchQueue.main.async {
                    downloadInFlight = false
                    if ok {
                        refreshLocalModelStatus()
                    } else {
                        downloadFailed = "Download failed. Check your connection and try again."
                    }
                }
            }
        }
    }

    /// Offered only for a whisper model that is on disk, has an encoder
    /// upstream, and a build that can use it.
    private var coremlRowVisible: Bool {
        !localBackendIsParakeet && !localBackendIsQwen && localModelReady
            && !downloadInFlight && coreml.available
    }

    private func startCoremlDownload() {
        guard !downloadInFlight, DimmyCore.shared.isInitialized else { return }
        let target = appState.localModel
        downloadInFlight = true
        downloadFailed = nil
        downloadingIsQwen = false
        downloadingIsParakeet = false
        // The core reports encoder progress under the whisper model's own
        // filename, so the existing per-file progress match works unchanged.
        downloadingTarget = target
        downloadingLabel = "Neural Engine encoder for \(target)"
        appState.modelDownloadProgress = 0
        appState.modelDownloadFilename = ""
        DispatchQueue.global(qos: .userInitiated).async {
            let ok = DimmyCore.shared.downloadCoremlEncoder(target)
            DispatchQueue.main.async {
                downloadInFlight = false
                if ok {
                    refreshLocalModelStatus()
                } else {
                    downloadFailed = "Neural Engine encoder download failed. Check your connection and try again."
                }
            }
        }
    }

    @ViewBuilder
    /// Progress of the download in flight. Whisper models are matched by
    /// filename, because the core reports one bar per file and a stale event
    /// from a previous model would otherwise drive this one. Parakeet and Qwen
    /// have a single bundle each, so their own flags are unambiguous.
    private var downloadingProgress: Double {
        if downloadingIsQwen { return appState.qwenDownloadProgress }
        if downloadingIsParakeet { return appState.parakeetDownloadProgress }
        guard appState.modelDownloadFilename == downloadingTarget else { return 0 }
        return appState.modelDownloadProgress
    }

    private func modelProgressRow(progress: Double, label: String) -> some View {
        MacRow(label, showsDivider: false) {
            HStack(spacing: 8) {
                ProgressView(value: progress)
                    .frame(width: 160)
                Text(String(format: "%.0f%%", progress * 100))
                    .font(.system(size: 12, design: .monospaced))
                    .foregroundStyle(Color.macTextSecondary)
                    .frame(width: 40, alignment: .trailing)
            }
        }
    }

    /// SecureField row revealed under "API key" when the user opts in via the
    /// Add/Replace button. Submitting (or pressing Save) writes `api_key`
    /// to the Rust core, then re-reads the config so `hasKey` flips.
    private var apiKeyEntryRow: some View {
        MacRow("Paste key", showsDivider: false) {
            SecureField("sk-...", text: $apiKeyInput)
                .textFieldStyle(.roundedBorder)
                .frame(width: 240)
                .onSubmit { saveApiKey() }
            Button("Save") { saveApiKey() }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .disabled(apiKeyInput.isEmpty)
        }
    }

    private func saveApiKey() {
        guard !apiKeyInput.isEmpty else { return }
        var config = appState.toRustConfig()
        config["api_key"] = apiKeyInput
        DimmyCore.shared.setConfig(config)
        apiKeyInput = ""
        showKeyField = false
        if let cfg = DimmyCore.shared.getConfig() {
            appState.loadFromRustConfig(cfg)
        }
    }

    // MARK: Microphone

    private var microphoneGroup: some View {
        Group {
            MacGroupLabel(text: "Microphone")
            MacTile {
                // Input device PROMOTED to Speech recognition (Simple).
                // This tile keeps only the gain knob now; the rest of
                // the audio chain lives in audioProcessingGroup below.
                MacRow(
                    "Microphone volume",
                    description: "50% default.",
                    hint: "Raises your input level so soft speech is picked up. Leave it low if your mic is already loud.",
                    hintURL: URL(string: "https://dimmy.app/help/audio-input"),
                    showsDivider: false
                ) {
                    // Slider runs over the same Rust-validated range
                    // (0.0...2.0, see save_config_file's assertion in
                    // core/src/lib.rs). 0.5 (= 50% default) matches
                    // the Rust default + the Win InputGainPercent
                    // alignment from commit c1896da. Display is the
                    // Rust value × 100 so the Settings number tracks
                    // the slider 1:1, no double-mapping.
                    Slider(
                        value: Binding(
                            get: { Double(appState.inputGain) },
                            set: { newValue in
                                appState.inputGain = Float(newValue)
                                persistConfig()
                            }
                        ),
                        in: 0.0...2.0,
                        step: 0.05
                    )
                    .frame(width: 160)
                    Text(String(format: "%.0f%%", Double(appState.inputGain) * 100))
                        .font(.system(size: 12, design: .monospaced))
                        .foregroundStyle(Color.macTextSecondary)
                        .frame(width: 44, alignment: .trailing)
                }
            }
        }
    }

    // MARK: Audio processing

    private var audioProcessingGroup: some View {
        Group {
            MacGroupLabel(text: "Audio processing")
            MacTile {
                MacRow(
                    "Preprocessing",
                    hint: "Runs a high pass filter, voice activity detection, and automatic gain control. Leave it on unless you are debugging audio.",
                    hintURL: URL(string: "https://dimmy.app/help/audio-input")
                ) {
                    Toggle("", isOn: Binding(
                        get: { appState.preprocessingEnabled },
                        set: { newValue in
                            appState.preprocessingEnabled = newValue
                            persistConfig()
                        }
                    ))
                    .toggleStyle(.switch)
                    .labelsHidden()
                }

                MacRow(
                    "Remove filler words",
                    hint: "Removes common filler words in 6 languages (including Italian 'cioè', 'ecc.') after transcription.",
                    hintURL: URL(string: "https://dimmy.app/help/filler-removal"),
                    showsDivider: appState.showAdvanced
                ) {
                    Toggle("", isOn: Binding(
                        get: { appState.fillerRemovalEnabled },
                        set: { newValue in
                            appState.fillerRemovalEnabled = newValue
                            persistConfig()
                        }
                    ))
                    .toggleStyle(.switch)
                    .labelsHidden()
                }

                if appState.showAdvanced {
                    MacRow(
                        "Chunk streaming",
                        description: "Parakeet local backend only.",
                        hint: "Transcribes while you speak, so the final text lands about 700 ms after you release the key instead of waiting for the whole clip. Needs the Parakeet local backend.",
                        showsDivider: appState.chunkStreamingEnabled
                            && appState.localSttBackend == "parakeet"
                    ) {
                        Toggle("", isOn: Binding(
                            get: { appState.chunkStreamingEnabled },
                            set: { newValue in
                                appState.chunkStreamingEnabled = newValue
                                persistConfig()
                            }
                        ))
                        .toggleStyle(.switch)
                        .labelsHidden()
                    }

                    // Live captions toggle, only meaningful when
                    // the chunked engine is firing AND the backend
                    // is Parakeet (Whisper.cpp is too slow per-chunk
                    // to keep up). Hide the row otherwise so it
                    // doesn't masquerade as a knob the user can flip.
                    if appState.chunkStreamingEnabled
                        && appState.localSttBackend == "parakeet" {
                        MacRow(
                            "Live captions",
                            hint: "Shows a floating caption while chunk streaming is on. Turn it off to keep the speed without showing text on screen.",
                            showsDivider: true
                        ) {
                            Toggle("", isOn: Binding(
                                get: { appState.liveCaptionsEnabled },
                                set: { newValue in
                                    appState.liveCaptionsEnabled = newValue
                                    persistConfig()
                                }
                            ))
                            .toggleStyle(.switch)
                            .labelsHidden()
                        }
                    }

                    // Call-detect nudge, 1 Hz CoreAudio poll surfaces
                    // a bottom-right popup when a VoIP call is detected.
                    // Off ⇒ no enumeration, no popup. Default on.
                    MacRow(
                        "Auto-detect meetings",
                        hint: "Polls the default microphone once a second and shows a bottom-right popup when a call lasts longer than 5 s. Per-app cooldown of 30 min after \"Not now\", permanent skip after \"Don't ask for this app\".",
                        showsDivider: !appState.callDetectExcludedApps.isEmpty
                    ) {
                        Toggle("", isOn: Binding(
                            get: { appState.callDetectEnabled },
                            set: { newValue in
                                appState.callDetectEnabled = newValue
                                persistConfig()
                            }
                        ))
                        .toggleStyle(.switch)
                        .labelsHidden()
                    }

                    // Exclusion list, apps the user picked "Don't ask
                    // again" for. Mirror of Win Settings exclusion card.
                    // Hidden when empty so the section doesn't clutter
                    // for users who never used the menu.
                    if !appState.callDetectExcludedApps.isEmpty {
                        let entries = appState.callDetectExcludedApps
                        ForEach(Array(entries.enumerated()), id: \.element) { idx, app in
                            MacRow(
                                Self.exclusionDisplayName(for: app),
                                description: "Auto-detect popup skipped.",
                                showsDivider: idx < entries.count - 1
                            ) {
                                Button("Remove") {
                                    appState.removeCallDetectExclusion(app)
                                }
                                .controlSize(.small)
                            }
                        }
                    }
                }
            }
        }
    }

    // MARK: Advanced, vocabulary / prompt

    private var advancedGroup: some View {
        Group {
            MacGroupLabel(text: "Vocabulary")
            MacTile {
                VStack(alignment: .leading, spacing: 8) {
                    Text("Custom vocabulary")
                        .font(.system(size: 13))
                    Text("Words and phrases the model should expect, names, acronyms, brand terms.")
                        .font(.system(size: 11))
                        .foregroundStyle(Color.macTextSecondary)
                    TextEditor(text: Binding(
                        get: { appState.prompt },
                        set: { newValue in
                            appState.prompt = newValue
                            persistConfig()
                        }
                    ))
                    .font(.system(size: 12, design: .monospaced))
                    .frame(minHeight: 80)
                    .padding(8)
                    .background(
                        RoundedRectangle(cornerRadius: 8, style: .continuous)
                            .fill(Color.primary.opacity(0.04))
                    )
                    .overlay(
                        RoundedRectangle(cornerRadius: 8, style: .continuous)
                            .stroke(Color.macControlStroke, lineWidth: 0.5)
                    )
                }
                .padding(EdgeInsets(top: 12, leading: 14, bottom: 12, trailing: 14))
            }
        }
    }

    // MARK: Bindings + persistence

    private var sttPresetBinding: Binding<String> {
        Binding(
            get: {
                SttPreset.find(url: appState.apiUrl, model: appState.apiModel)?.id
                    ?? "groq-whisper-turbo"
            },
            set: { newValue in
                if let preset = SttPreset.presets.first(where: { $0.id == newValue }) {
                    appState.sttProvider = preset.provider
                    appState.apiUrl = preset.apiUrl
                    appState.apiModel = preset.model
                    persistConfig()
                }
            }
        )
    }

    /// Send the AppState diff back to Rust core. Same FFI plumbing the
    /// legacy views use, just hoisted here so each page doesn't duplicate
    /// the boilerplate.
    private func persistConfig() {
        let json = appState.toRustConfig()
        DimmyCore.shared.setConfig(json)
    }
}

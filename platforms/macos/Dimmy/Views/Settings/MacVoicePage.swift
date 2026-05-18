import SwiftUI

// Voice input — STT mode + provider + API key + language + microphone +
// audio processing. Consolidates the legacy `General` and `Models` tabs
// into a single page matching the design handoff `MacVoice` component.

struct MacVoicePage: View {
    @ObservedObject var appState: AppState

    @State private var apiKeyInput: String = ""
    @State private var showKeyField: Bool = false

    @State private var localModelExists: Bool = false
    @State private var downloadInFlight: Bool = false
    @State private var downloadFailed: String? = nil

    /// Text-field state for the "add a word" row in the custom-dictionary
    /// section. Kept inline to avoid a parallel view-model class — the
    /// list itself lives on AppState and `addDictWord` calls the FFI so
    /// the Rust core remains the single writer.
    @State private var newDictWord: String = ""
    @State private var dictAddError: String? = nil

    /// Sentinel value for the Parakeet entry in the unified local-model
    /// Picker. Mirrors `ParakeetTag` in the Windows OnboardingWindow.xaml.cs
    /// so the two UIs round-trip the same selection through the Rust core.
    private static let parakeetTag = "parakeet:fp32"

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            speechRecognitionGroup
            microphoneGroup
            audioProcessingGroup
            customDictionaryGroup

            if appState.showAdvanced {
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
                                            .fill(Color.black.opacity(0.04))
                                    )
                                }
                            }
                        }
                        .frame(maxHeight: 180)
                    }
                }
                .padding(EdgeInsets(top: 12, leading: 14, bottom: 12, trailing: 14))
            }
            MacGroupFooter(text: "Words listed here are passed to the STT engine on every transcription to bias recognition. Cloud (Whisper / Gemini), Deepgram keyterms, and local Whisper all honour this; Parakeet ignores it (NeMo TDT has no boost-word API).")
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
            dictAddError = "“\(trimmed)” is already in the dictionary"
            newDictWord = ""
        case .error:
            dictAddError = "Could not add — check log"
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
                MacRow("Mode", description: "Where transcription runs") {
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
                    MacRow(
                        "Provider",
                        description: "≈250 ms typical latency"
                    ) {
                        Picker("", selection: sttPresetBinding) {
                            ForEach(SttPreset.presets) { preset in
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

                    MacRow(
                        "API key",
                        description: "Stored locally · AES-256-GCM",
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
                        Button(appState.hasKey ? (showKeyField ? "Cancel" : "Replace…")
                                               : (showKeyField ? "Cancel" : "Add key…")) {
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
                        "Local model",
                        description: localBackendIsParakeet
                            ? "NVIDIA Parakeet TDT v3 — Apple Neural Engine, fastest"
                            : "Whisper, runs entirely offline",
                        showsDivider: !localModelReady || downloadInFlight
                    ) {
                        Picker("", selection: localModelPickerBinding) {
                            Text("Tiny · 78 MB").tag("ggml-tiny-q8_0.bin")
                            Text("Base · 142 MB").tag("ggml-base-q8_0.bin")
                            Text("Small · 466 MB").tag("ggml-small-q8_0.bin")
                            Text("Medium · 1.5 GB").tag("ggml-medium-q8_0.bin")
                            Text("Parakeet TDT v3 · 466 MB · Apple Neural Engine")
                                .tag(Self.parakeetTag)
                        }
                        .labelsHidden()
                        .frame(width: 260)
                    }

                    if downloadInFlight {
                        modelProgressRow(
                            progress: localBackendIsParakeet
                                ? appState.parakeetDownloadProgress
                                : appState.modelDownloadProgress,
                            label: localBackendIsParakeet
                                ? "Downloading Parakeet CoreML bundle (~466 MB)…"
                                : "Downloading \(appState.localModel)…"
                        )
                    } else if !localModelReady {
                        MacRow(
                            "Download",
                            description: downloadFailed ?? (localBackendIsParakeet
                                ? "Parakeet CoreML bundle (~466 MB) isn't on disk yet."
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
                    }
                }

                MacRow("Language", showsDivider: false) {
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
            }
            MacGroupFooter(text: "Helps the model with rare words and names. Auto-detect identifies the language from your first sentence — but tends to misfire on clips shorter than two seconds.")
        }
        .onAppear { refreshLocalModelStatus() }
    }

    private var localBackendIsParakeet: Bool {
        appState.localSttBackend == "parakeet"
    }

    /// True when the currently-selected local backend has its data on
    /// disk and is ready to transcribe. Whisper: ggml file present.
    /// Parakeet: full ~2.5 GB bundle present.
    private var localModelReady: Bool {
        if localBackendIsParakeet {
            return appState.parakeetBundlePresent
        }
        return localModelExists
    }

    /// Single Picker binding that drives BOTH `localModel` (whisper
    /// filename) and `localSttBackend` ("whisper" | "parakeet"). Picking
    /// the Parakeet sentinel flips the backend without overwriting the
    /// remembered ggml choice, so toggling back restores the previous
    /// whisper model — same UX as the Windows ComboBox unification.
    private var localModelPickerBinding: Binding<String> {
        Binding(
            get: {
                localBackendIsParakeet ? Self.parakeetTag : appState.localModel
            },
            set: { newValue in
                if newValue == Self.parakeetTag {
                    appState.localSttBackend = "parakeet"
                    // Auto-enable chunk streaming on Parakeet pick. Mirror
                    // of SettingsWindow.xaml.cs:LocalModel_SelectionChanged
                    // on Windows — chunk streaming is Parakeet-only at
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
            return
        }
        // Move the FFI probes off the main thread. Each call is a
        // stat() / directory walk in Rust; individually cheap (<10 ms)
        // but when this runs synchronously inside `.onAppear` the
        // first tab-switch into Voice blocks the main thread before
        // SwiftUI can render the page — visible as a "slow click"
        // on the sidebar item. Dispatching them async lets the
        // page paint immediately and the model status fills in a
        // few milliseconds later.
        let modelName = appState.localModel
        DispatchQueue.global(qos: .userInitiated).async {
            let exists = DimmyCore.shared.modelExists(modelName)
            let parakeet = DimmyCore.shared.parakeetBundlePresent()
            DispatchQueue.main.async {
                self.localModelExists = exists
                self.appState.parakeetBundlePresent = parakeet
                self.downloadFailed = nil
            }
        }
    }

    private func startSttDownload() {
        guard !downloadInFlight, DimmyCore.shared.isInitialized else { return }
        downloadInFlight = true
        downloadFailed = nil
        if localBackendIsParakeet {
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
            appState.modelDownloadProgress = 0
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

    @ViewBuilder
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
            SecureField("sk-…", text: $apiKeyInput)
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
                MacRow("Input device") {
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

                MacRow(
                    "Microphone volume",
                    description: "Software gain applied before transcription · 50% default",
                    showsDivider: false
                ) {
                    // Slider runs over the same Rust-validated range
                    // (0.0...2.0 — see save_config_file's assertion in
                    // core/src/lib.rs). 0.5 (= 50% default) matches
                    // the Rust default + the Win InputGainPercent
                    // alignment from commit c1896da. Display is the
                    // Rust value × 100 so the Settings number tracks
                    // the slider 1:1 — no double-mapping.
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
                    description: "High-pass + VAD + AGC before transcribing"
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
                    description: "Strips um, uh, basically, cioè, ecc. across 6 languages",
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
                        description: "Stream audio in 5 s chunks (Parakeet only)",
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

                    // Live captions toggle — only meaningful when
                    // the chunked engine is firing AND the backend
                    // is Parakeet (Whisper.cpp is too slow per-chunk
                    // to keep up). Hide the row otherwise so it
                    // doesn't masquerade as a knob the user can flip.
                    if appState.chunkStreamingEnabled
                        && appState.localSttBackend == "parakeet" {
                        MacRow(
                            "Live captions",
                            description: "Floating subtitle window during recording",
                            showsDivider: false
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
                }
            }
        }
    }

    // MARK: Advanced — vocabulary / prompt

    private var advancedGroup: some View {
        Group {
            MacGroupLabel(text: "Vocabulary")
            MacTile {
                VStack(alignment: .leading, spacing: 8) {
                    Text("Custom vocabulary")
                        .font(.system(size: 13))
                    Text("Words and phrases the model should expect — names, acronyms, brand terms.")
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
                            .fill(Color.black.opacity(0.04))
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

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

    /// Sentinel value for the Parakeet entry in the unified local-model
    /// Picker. Mirrors `ParakeetTag` in the Windows OnboardingWindow.xaml.cs
    /// so the two UIs round-trip the same selection through the Rust core.
    private static let parakeetTag = "parakeet:fp32"

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            speechRecognitionGroup
            microphoneGroup
            audioProcessingGroup

            if appState.showAdvanced {
                advancedGroup
            }
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
                                            .frame(width: 12, height: 12)
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
        // FFI sync calls, but cheap (just stat() on the files).
        localModelExists = DimmyCore.shared.modelExists(appState.localModel)
        appState.parakeetBundlePresent = DimmyCore.shared.parakeetBundlePresent()
        downloadFailed = nil
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

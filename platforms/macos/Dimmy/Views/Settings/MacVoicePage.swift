import SwiftUI

// Voice input — STT mode + provider + API key + language + microphone +
// audio processing. Consolidates the legacy `General` and `Models` tabs
// into a single page matching the design handoff `MacVoice` component.

struct MacVoicePage: View {
    @ObservedObject var appState: AppState

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
                                Text(preset.displayName).tag(preset.id)
                            }
                        }
                        .labelsHidden()
                        .frame(width: 280)
                    }

                    MacRow(
                        "API key",
                        description: "Stored in your Keychain · AES-256-GCM"
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
                        Button("Replace…") {
                            // Phase 4: dedicated Replace flow. Today the
                            // legacy GeneralSettingsView owns the password
                            // entry — wiring deferred until that view's
                            // input is ported into the Tahoe flow.
                        }
                        .controlSize(.small)
                    }
                } else {
                    MacRow(
                        "Local model",
                        description: "Whisper, runs entirely offline"
                    ) {
                        Picker("", selection: Binding(
                            get: { appState.localModel },
                            set: { newValue in
                                appState.localModel = newValue
                                persistConfig()
                            }
                        )) {
                            Text("Tiny · 78 MB").tag("ggml-tiny-q8_0.bin")
                            Text("Base · 142 MB").tag("ggml-base-q8_0.bin")
                            Text("Small · 466 MB").tag("ggml-small-q8_0.bin")
                            Text("Medium · 1.5 GB").tag("ggml-medium-q8_0.bin")
                        }
                        .labelsHidden()
                        .frame(width: 200)
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
                    description: "Software gain applied before transcription",
                    showsDivider: false
                ) {
                    Slider(
                        value: Binding(
                            get: { Double(appState.inputGain) },
                            set: { newValue in
                                appState.inputGain = Float(newValue)
                                persistConfig()
                            }
                        ),
                        in: 0.5...1.5
                    )
                    .frame(width: 160)
                    Text(String(format: "%.0f%%", Double(appState.inputGain) * 100))
                        .font(.system(size: 12, design: .monospaced))
                        .foregroundStyle(Color.macTextSecondary)
                        .frame(width: 36, alignment: .trailing)
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
                        description: "Stream audio in 250 ms chunks for partial results",
                        showsDivider: false
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

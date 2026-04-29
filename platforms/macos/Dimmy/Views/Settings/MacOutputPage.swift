import SwiftUI

// Output — LLM enhancement style + provider + paste behaviour. Style
// chips replace the old Picker — coloured swatches make the dozen
// options scannable. Sub-mode (cloud / on-device) and provider routing
// follow the design's two-tier layout.

struct MacOutputPage: View {
    @ObservedObject var appState: AppState

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            rewriteStyleGroup
            llmModeGroup
            pasteboardGroup

            if appState.showAdvanced {
                advancedLlmGroup
            }
        }
    }

    // MARK: Rewrite style (chip picker)

    private var rewriteStyleGroup: some View {
        Group {
            MacGroupLabel(text: "Rewrite style")
            MacTile {
                VStack(alignment: .leading, spacing: 12) {
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Style").font(.system(size: 13))
                        Text("After transcribing, Dimmy rewrites your text before pasting.")
                            .font(.system(size: 11))
                            .foregroundStyle(Color.macTextSecondary)
                    }
                    chipFlow
                }
                .padding(EdgeInsets(top: 12, leading: 14, bottom: 12, trailing: 14))
            }
        }
    }

    /// Flexbox-style wrap of style chips. SwiftUI doesn't have native
    /// flex-wrap, so we lay out manually using `LazyVGrid` with adaptive
    /// columns sized to the chip width.
    private var chipFlow: some View {
        LazyVGrid(
            columns: [GridItem(.adaptive(minimum: 110), spacing: 6)],
            alignment: .leading,
            spacing: 6
        ) {
            ForEach(MacLlmStyles, id: \.key) { entry in
                MacChip(
                    label: entry.label,
                    color: MacStyleColor.color(for: entry.key),
                    selected: appState.llmStyle == entry.key
                ) {
                    appState.llmStyle = entry.key
                    appState.llmEnabled = entry.key != "off"
                    persistConfig()
                }
            }
        }
    }

    // MARK: LLM mode + provider

    private var llmModeGroup: some View {
        Group {
            MacGroupLabel(text: "LLM provider")
            MacTile {
                MacRow("Mode", description: "Where the rewrite runs") {
                    Picker("", selection: Binding(
                        get: { appState.llmMode },
                        set: { newValue in
                            appState.llmMode = newValue
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

                if appState.llmMode == "cloud" {
                    MacRow("Provider") {
                        Picker("", selection: llmPresetBinding) {
                            ForEach(LlmPreset.presets) { preset in
                                Text(preset.displayName).tag(preset.id)
                            }
                        }
                        .labelsHidden()
                        .frame(width: 280)
                    }

                    MacRow(
                        "Use same key as STT",
                        description: "Reuse the speech-to-text key for the LLM provider when both are the same vendor.",
                        showsDivider: !appState.llmUseSameKey
                    ) {
                        Toggle("", isOn: Binding(
                            get: { appState.llmUseSameKey },
                            set: { newValue in
                                appState.llmUseSameKey = newValue
                                persistConfig()
                            }
                        ))
                        .toggleStyle(.switch)
                        .labelsHidden()
                    }

                    if !appState.llmUseSameKey {
                        MacRow(
                            "LLM API key",
                            description: "Encrypted locally. Used when not sharing with STT.",
                            showsDivider: false
                        ) {
                            if appState.hasLlmKey {
                                HStack(spacing: 4) {
                                    Image(systemName: "checkmark.circle.fill")
                                        .foregroundStyle(.green)
                                    Text("Saved")
                                        .font(.system(size: 12, weight: .medium))
                                        .foregroundStyle(.green)
                                }
                            }
                            Button("Replace…") {}
                                .controlSize(.small)
                        }
                    }
                } else {
                    MacRow(
                        "Local model",
                        description: "Runs entirely on your device",
                        showsDivider: false
                    ) {
                        Picker("", selection: Binding(
                            get: { appState.localLlmModel },
                            set: { newValue in
                                appState.localLlmModel = newValue
                                persistConfig()
                            }
                        )) {
                            Text("Gemma 4 E2B-it (Q4)").tag("gemma-4-E2B-it-Q4_K_M.gguf")
                            Text("Gemma 4 E4B-it (Q4)").tag("gemma-4-E4B-it-Q4_K_M.gguf")
                        }
                        .labelsHidden()
                        .frame(width: 220)
                    }
                }
            }
        }
    }

    // MARK: Pasteboard

    private var pasteboardGroup: some View {
        Group {
            MacGroupLabel(text: "Pasteboard")
            MacTile {
                MacRow(
                    "Keep in clipboard history",
                    description: "Leave the transcription on the clipboard after auto-paste so you can paste it again later.",
                    showsDivider: false
                ) {
                    Toggle("", isOn: Binding(
                        get: { appState.keepInClipboard },
                        set: { newValue in
                            appState.keepInClipboard = newValue
                            persistConfig()
                        }
                    ))
                    .toggleStyle(.switch)
                    .labelsHidden()
                }
            }
        }
    }

    // MARK: Advanced — tone, translate, custom prompt

    private var advancedLlmGroup: some View {
        Group {
            MacGroupLabel(text: "Advanced LLM")
            MacTile {
                MacRow("Tone", description: "Adjusts how formal or casual the rewrite sounds") {
                    Picker("", selection: Binding(
                        get: { appState.llmTone },
                        set: { newValue in
                            appState.llmTone = newValue
                            persistConfig()
                        }
                    )) {
                        ForEach(LlmTone.allCases) { tone in
                            Text(tone.displayName).tag(tone.rawValue)
                        }
                    }
                    .labelsHidden()
                    .frame(width: 160)
                }

                MacRow(
                    "Translate output to",
                    description: "Translate the LLM rewrite to this language. The pill's scroll wheel changes the same setting."
                ) {
                    Picker("", selection: Binding(
                        get: { appState.llmTranslateTo },
                        set: { newValue in
                            appState.llmTranslateTo = newValue
                            persistConfig()
                        }
                    )) {
                        Text("No translation").tag("")
                        Text("Italiano").tag("it")
                        Text("English").tag("en")
                        Text("Español").tag("es")
                        Text("Français").tag("fr")
                        Text("Deutsch").tag("de")
                        Text("Português").tag("pt")
                    }
                    .labelsHidden()
                    .frame(width: 180)
                }

                VStack(alignment: .leading, spacing: 8) {
                    Text("Custom prompt")
                        .font(.system(size: 13))
                    Text("Free-form instruction prepended to the LLM prompt for the Custom style.")
                        .font(.system(size: 11))
                        .foregroundStyle(Color.macTextSecondary)
                    TextEditor(text: Binding(
                        get: { appState.llmCustomPrompt },
                        set: { newValue in
                            appState.llmCustomPrompt = newValue
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

    private var llmPresetBinding: Binding<String> {
        Binding(
            get: {
                LlmPreset.find(url: appState.llmApiUrl, model: appState.llmApiModel)?.id
                    ?? "groq-llama70b"
            },
            set: { newValue in
                if let preset = LlmPreset.presets.first(where: { $0.id == newValue }) {
                    appState.llmApiUrl = preset.apiUrl
                    appState.llmApiModel = preset.model
                    persistConfig()
                }
            }
        )
    }

    private func persistConfig() {
        DimmyCore.shared.setConfig(appState.toRustConfig())
    }
}

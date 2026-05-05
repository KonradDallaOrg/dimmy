import SwiftUI

struct OutputSettingsView: View {
    @ObservedObject var appState: AppState
    // Local state for LLM preset picker — avoids computed binding issues
    @State private var selectedLlmPreset: String = ""

    var body: some View {
        Form {
            // MARK: - LLM Enhancement

            Section("LLM Enhancement") {
                Toggle("Enable LLM post-processing", isOn: $appState.llmEnabled)
                    .onChange(of: appState.llmEnabled) { _, _ in syncConfigToRust() }

                if appState.llmEnabled {
                    Picker("Style", selection: $appState.llmStyle) {
                        ForEach(LlmStyle.allCases) { style in
                            HStack {
                                Circle()
                                    .fill(style.color)
                                    .frame(width: 8, height: 8)
                                Text(style.displayName)
                            }
                            .tag(style.rawValue)
                        }
                    }
                    .onChange(of: appState.llmStyle) { _, _ in syncConfigToRust() }
                }
            }

            if appState.llmEnabled {
                Section("LLM Mode") {
                    Picker("Mode", selection: $appState.llmMode) {
                        Text("Local (Offline)").tag("local")
                        Text("Cloud").tag("cloud")
                    }
                    .pickerStyle(.segmented)
                    .onChange(of: appState.llmMode) { _, _ in syncConfigToRust() }

                    if appState.llmMode == "local" {
                        LLMModelSettingsView()
                    }
                }
            }

            if appState.llmEnabled && appState.llmMode == "cloud" {
                Section("LLM Provider") {
                    Picker("Provider + Model", selection: $selectedLlmPreset) {
                        ForEach(LlmPreset.presets) { preset in
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
                    .onChange(of: selectedLlmPreset) { _, newId in
                        applyLlmPreset(newId)
                    }

                    if selectedLlmPreset == "custom" {
                        TextField("API URL", text: $appState.llmApiUrl)
                            .textFieldStyle(.roundedBorder)
                            .onSubmit { syncConfigToRust() }

                        TextField("Model", text: $appState.llmApiModel)
                            .textFieldStyle(.roundedBorder)
                            .onSubmit { syncConfigToRust() }
                    }

                    llmApiKeyRow
                }
            }

            Section("Clipboard") {
                Toggle("Keep text in clipboard (don't auto-paste)", isOn: $appState.keepInClipboard)
                    .onChange(of: appState.keepInClipboard) { _, _ in syncConfigToRust() }

                if !appState.keepInClipboard {
                    Text("Text is pasted via \u{2318}V and clipboard is restored after")
                        .font(.system(size: 11))
                        .foregroundColor(.secondary)
                } else {
                    Text("Text is copied to clipboard but NOT auto-pasted")
                        .font(.system(size: 11))
                        .foregroundColor(.secondary)
                }
            }

            // MARK: - Advanced sections

            if appState.showAdvanced {
                if appState.llmEnabled {
                    Section("LLM Options") {
                        Picker("Tone", selection: $appState.llmTone) {
                            ForEach(LlmTone.allCases) { tone in
                                Text(tone.displayName).tag(tone.rawValue)
                            }
                        }
                        .onChange(of: appState.llmTone) { _, _ in syncConfigToRust() }

                        Picker("Translate to", selection: $appState.llmTranslateTo) {
                            Text("None").tag("none")
                            ForEach(AppState.languageMap, id: \.code) { item in
                                if !item.code.isEmpty {
                                    Text(item.display).tag(item.display)
                                }
                            }
                        }
                        .onChange(of: appState.llmTranslateTo) { _, _ in syncConfigToRust() }

                        if appState.llmStyleEnum == .custom {
                            VStack(alignment: .leading, spacing: 4) {
                                Text("Custom Prompt")
                                    .font(.system(size: 12, weight: .medium))
                                TextEditor(text: $appState.llmCustomPrompt)
                                    .frame(height: 80)
                                    .font(.system(size: 12))
                                    .border(Color.secondary.opacity(0.3))
                            }
                            .onChange(of: appState.llmCustomPrompt) { _, _ in syncConfigToRust() }
                        }

                        Toggle("Log LLM requests", isOn: $appState.llmLogEnabled)
                            .onChange(of: appState.llmLogEnabled) { _, _ in syncConfigToRust() }

                        Toggle("Use same API key as STT", isOn: $appState.llmUseSameKey)
                            .onChange(of: appState.llmUseSameKey) { _, _ in syncConfigToRust() }

                        if !appState.llmUseSameKey {
                            separateLlmApiKeyRow
                        }
                    }
                }
            }
        }
        .formStyle(.grouped)
        .onAppear {
            // Init local LLM preset from current config
            selectedLlmPreset = LlmPreset.find(url: appState.llmApiUrl, model: appState.llmApiModel)?.id ?? "custom"
        }
    }

    // MARK: - LLM Preset

    private func applyLlmPreset(_ presetId: String) {
        guard let preset = LlmPreset.presets.first(where: { $0.id == presetId }) else { return }
        if preset.id != "custom" {
            appState.llmApiUrl = preset.apiUrl
            appState.llmApiModel = preset.model
        }
        syncConfigToRust()
    }

    // MARK: - LLM API Key Row

    @State private var llmKeyInput: String = ""
    @State private var showLlmKeyField: Bool = false

    private var llmApiKeyRow: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Image(systemName: appState.hasKey ? "checkmark.circle.fill" : "xmark.circle")
                    .foregroundColor(appState.hasKey ? .green : .red)
                Text(appState.hasKey ? "API key configured" : "No API key")
                    .font(.system(size: 12))
                Spacer()
                Button(appState.hasKey ? "Change" : "Add Key") {
                    showLlmKeyField.toggle()
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
            }

            if showLlmKeyField {
                HStack {
                    SecureField("Paste API key...", text: $llmKeyInput)
                        .textFieldStyle(.roundedBorder)
                    Button("Save") {
                        guard !llmKeyInput.isEmpty else { return }
                        var config = appState.toRustConfig()
                        config["api_key"] = llmKeyInput
                        DimmyCore.shared.setConfig(config)
                        llmKeyInput = ""
                        showLlmKeyField = false
                        if let cfg = DimmyCore.shared.getConfig() {
                            appState.loadFromRustConfig(cfg)
                        }
                    }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.small)
                }
            }
        }
    }

    // MARK: - Separate LLM API Key Row (advanced)

    @State private var separateLlmKeyInput: String = ""
    @State private var showSeparateLlmKeyField: Bool = false

    private var separateLlmApiKeyRow: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Image(systemName: appState.hasLlmKey ? "checkmark.circle.fill" : "xmark.circle")
                    .foregroundColor(appState.hasLlmKey ? .green : .red)
                Text(appState.hasLlmKey ? "Separate LLM key configured" : "No separate LLM key")
                    .font(.system(size: 12))
                Spacer()
                Button(appState.hasLlmKey ? "Change" : "Add Key") {
                    showSeparateLlmKeyField.toggle()
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
            }

            if showSeparateLlmKeyField {
                HStack {
                    SecureField("Paste LLM API key...", text: $separateLlmKeyInput)
                        .textFieldStyle(.roundedBorder)
                    Button("Save") {
                        guard !separateLlmKeyInput.isEmpty else { return }
                        var config = appState.toRustConfig()
                        config["llm_api_key"] = separateLlmKeyInput
                        DimmyCore.shared.setConfig(config)
                        separateLlmKeyInput = ""
                        showSeparateLlmKeyField = false
                        if let cfg = DimmyCore.shared.getConfig() {
                            appState.loadFromRustConfig(cfg)
                        }
                    }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.small)
                }
            }
        }
    }

    private func syncConfigToRust() {
        DimmyCore.shared.setConfig(appState.toRustConfig())
    }
}

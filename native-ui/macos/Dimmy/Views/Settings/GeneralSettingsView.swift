import SwiftUI
import AppKit

struct GeneralSettingsView: View {
    @ObservedObject var appState: AppState
    // Local state for STT preset picker — avoids computed binding issues
    @State private var selectedSttPreset: String = ""

    var body: some View {
        Form {
            // MARK: - Default section: Language, API key, Theme
            Section("Transcription") {
                Picker("Language", selection: $appState.selectedLanguage) {
                    ForEach(appState.languages, id: \.self) { lang in
                        Text(lang).tag(lang)
                    }
                }
                .onChange(of: appState.selectedLanguage) { _, _ in syncConfigToRust() }
            }

            Section("API Key") {
                apiKeyRow
            }

            Section("Appearance") {
                Picker("Theme", selection: $appState.theme) {
                    ForEach(AppTheme.allCases, id: \.self) { theme in
                        Text(theme.rawValue).tag(theme)
                    }
                }
                .pickerStyle(.segmented)
                .onChange(of: appState.theme) { _, newTheme in
                    applyTheme(newTheme)
                }
            }

            // MARK: - Advanced sections
            if appState.showAdvanced {
                Section("STT Provider") {
                    Picker("Provider + Model", selection: $selectedSttPreset) {
                        ForEach(SttPreset.presets) { preset in
                            Text(preset.displayName).tag(preset.id)
                        }
                    }
                    .onChange(of: selectedSttPreset) { _, newId in
                        applySttPreset(newId)
                    }

                    if selectedSttPreset == "custom" {
                        TextField("API URL", text: $appState.apiUrl)
                            .textFieldStyle(.roundedBorder)
                            .onSubmit { syncConfigToRust() }

                        TextField("Model", text: $appState.apiModel)
                            .textFieldStyle(.roundedBorder)
                            .onSubmit { syncConfigToRust() }
                    }
                }

                Section("Audio Input") {
                    if appState.devices.isEmpty {
                        Text("No audio devices found")
                            .foregroundColor(.secondary)
                    } else {
                        Picker("Microphone", selection: Binding(
                            get: { appState.selectedDevice ?? appState.devices.first ?? "" },
                            set: { appState.selectedDevice = $0; syncConfigToRust() }
                        )) {
                            ForEach(appState.devices, id: \.self) { device in
                                Text(device).tag(device)
                            }
                        }
                    }

                    HStack {
                        Text("Input Gain")
                        Slider(value: $appState.inputGain, in: 0.0...2.0, step: 0.1)
                        Text(String(format: "%.1fx", appState.inputGain))
                            .monospacedDigit()
                            .frame(width: 36, alignment: .trailing)
                    }
                    .onChange(of: appState.inputGain) { _, _ in syncConfigToRust() }

                    Toggle("Audio Preprocessing (VAD + noise reduction)", isOn: $appState.preprocessingEnabled)
                        .onChange(of: appState.preprocessingEnabled) { _, _ in syncConfigToRust() }

                    Toggle("Chunk Streaming", isOn: $appState.chunkStreamingEnabled)
                        .onChange(of: appState.chunkStreamingEnabled) { _, _ in syncConfigToRust() }
                }

                Section("Security") {
                    Toggle("Use macOS Keychain for API keys", isOn: $appState.useKeyring)
                        .onChange(of: appState.useKeyring) { _, _ in syncConfigToRust() }

                    if appState.useKeyring {
                        Text("Store API keys in macOS Keychain (more secure). When enabled, macOS may ask for your login password to save the key securely.")
                            .font(.system(size: 11))
                            .foregroundColor(.secondary)
                    } else {
                        Text("API keys are stored in an encrypted local config file.")
                            .font(.system(size: 11))
                            .foregroundColor(.secondary)
                    }
                }
            }
        }
        .formStyle(.grouped)
        .onAppear {
            appState.devices = DimmyCore.shared.listDevices()
            // Ensure initial language is valid
            if !appState.languages.contains(appState.selectedLanguage) {
                appState.selectedLanguage = "Auto Detect"
            }
            // Init local STT preset from current config
            selectedSttPreset = SttPreset.find(url: appState.apiUrl, model: appState.apiModel)?.id ?? "custom"
            applyTheme(appState.theme)
        }
    }

    // MARK: - STT Preset

    private func applySttPreset(_ presetId: String) {
        guard let preset = SttPreset.presets.first(where: { $0.id == presetId }) else { return }
        appState.sttProvider = preset.provider
        if preset.id != "custom" {
            appState.apiUrl = preset.apiUrl
            appState.apiModel = preset.model
        }
        syncConfigToRust()
    }

    // MARK: - Theme

    private func applyTheme(_ theme: AppTheme) {
        switch theme {
        case .auto:  NSApp.appearance = nil
        case .light: NSApp.appearance = NSAppearance(named: .aqua)
        case .dark:  NSApp.appearance = NSAppearance(named: .darkAqua)
        }
    }

    // MARK: - API Key Row

    @State private var apiKeyInput: String = ""
    @State private var showKeyField: Bool = false

    private var apiKeyRow: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Image(systemName: appState.hasKey ? "checkmark.circle.fill" : "xmark.circle")
                    .foregroundColor(appState.hasKey ? .green : .red)
                Text(appState.hasKey ? "API key configured" : "No API key")
                    .font(.system(size: 12))
                Spacer()
                Button(appState.hasKey ? "Change" : "Add Key") {
                    showKeyField.toggle()
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
            }

            if showKeyField {
                HStack {
                    SecureField("Paste API key...", text: $apiKeyInput)
                        .textFieldStyle(.roundedBorder)
                    Button("Save") {
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

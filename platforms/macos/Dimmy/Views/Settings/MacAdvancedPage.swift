import SwiftUI

// Advanced — diagnostics + GPU acceleration. Visible only when the
// sidebar Advanced toggle is ON (gated by `MacSettingsTab.advanced`
// filter in MacSettingsContainerView.filteredTabs).

struct MacAdvancedPage: View {
    @ObservedObject var appState: AppState

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            MacNote(
                title: "Advanced settings",
                message: "These options can affect performance and stability. Change them only if you know what you're doing.",
                systemImage: "exclamationmark.triangle.fill"
            )
            .padding(.bottom, 8)

            appearanceGroup
            performanceGroup
            recapModelGroup
            autoRecapGroup
            #if DEBUG
            debugSeedGroup
            #endif
            diagnosticsGroup
            resetGroup
        }
    }

    #if DEBUG
    /// Debug-only: drop one sample row with a synthesised WAV so the
    /// History detail's waveform + Raw/Enhanced toggle can be demoed
    /// without a microphone. Compiled out in Release/Staging — under
    /// `#if DEBUG` instead of a runtime flag so a binary you ship
    /// physically cannot insert fake rows.
    private var debugSeedGroup: some View {
        Group {
            MacGroupLabel(text: "Debug · history seed")
            MacTile {
                MacRow(
                    "Insert sample row",
                    description: "Adds one fake history entry with a synthesised 4 s WAV so the waveform + playback can be exercised without a real recording.",
                    showsDivider: false
                ) {
                    Button("Add sample") {
                        seedSampleHistory()
                    }
                    .controlSize(.small)
                }
            }
        }
    }

    private func seedSampleHistory() {
        DispatchQueue.global(qos: .utility).async {
            if !DimmyCore.shared.isInitialized {
                _ = DimmyCore.shared.initialize()
            }
            guard DimmyCore.shared.isInitialized else { return }

            let id = DimmyCore.shared.historySave(
                text: "today we agreed on a three-tier pricing structure, marketing will draft launch copy by friday, engineering needs a feature flag for the trial gate",
                language: "en",
                duration: 4.0
            )
            guard id > 0 else { return }

            DimmyCore.shared.historyUpdateEnhanced(
                id: id,
                text: "Today we agreed on a three-tier pricing structure. Marketing will draft launch copy by Friday. Engineering needs a feature flag for the trial gate.\n\n═════ Recap ═════\n• 3-tier pricing structure agreed\n• Marketing draft due Friday\n• Engineering: feature flag for trial gate"
            )

            if let dir = SampleAudioSynth.historyAudioDir() {
                let wavURL = dir.appendingPathComponent("\(id).wav")
                if let size = SampleAudioSynth.writeBurstyWAV(to: wavURL) {
                    DimmyCore.shared.historyUpdateAudio(
                        id: id, path: wavURL.path, sizeBytes: size
                    )
                }
            }
        }
    }
    #endif

    /// Curated model picker for the meeting recap + auto-recap. Empty
    /// "Auto" entry preserves the existing URL-based heuristic
    /// (Anthropic→Opus, Gemini→Pro, else user's configured model).
    /// Mirror of Win SettingsWindow.xaml recap-model card.
    ///
    /// Footgun (same as Win): the recap shares the LLM API URL + key
    /// with dictation. Picking a Gemini model with Anthropic configured
    /// → 400 invalid_request_error. The note below warns the user.
    /// Multi-provider keystore tracked separately.
    private var recapModelGroup: some View {
        Group {
            MacGroupLabel(text: "Meeting recap model")
            MacTile {
                MacRow(
                    "Recap model",
                    description: "Used for the meeting recap pipeline and the long-dictation auto-recap. Auto matches your dictation provider (Anthropic→Opus, Gemini→Pro). Pick a specific model only if it matches your configured LLM URL/key.",
                    showsDivider: false
                ) {
                    Picker("", selection: Binding<String>(
                        get: { appState.recapModelOverride },
                        set: { newValue in
                            appState.recapModelOverride = newValue
                            DimmyCore.shared.setConfig(appState.toRustConfig())
                        }
                    )) {
                        ForEach(RecapModelOption.curated) { opt in
                            Label(opt.label, systemImage: opt.iconName)
                                .tag(opt.id)
                        }
                        // Render any custom value users may have hand-
                        // edited in config.json so it's selectable
                        // without being lost on save.
                        if !appState.recapModelOverride.isEmpty,
                           !RecapModelOption.curated.contains(where: { $0.id == appState.recapModelOverride }) {
                            Divider()
                            Label("Custom — \(appState.recapModelOverride)",
                                  systemImage: "wrench.adjustable")
                                .tag(appState.recapModelOverride)
                        }
                    }
                    .labelsHidden()
                    .pickerStyle(.menu)
                    .frame(minWidth: 280)
                }
            }
            MacGroupFooter(text: "The recap call uses your configured LLM API URL + key. Picking a model from a different provider will fail with HTTP 400. Multi-provider key storage is on the roadmap.")
        }
    }

    /// Phase 6.4 — fire-and-forget recap on long dictations. Independent
    /// of meeting mode (which always recaps) and from the dictation
    /// rewrite (which uses llm_style). 0 = disabled.
    private var autoRecapGroup: some View {
        Group {
            MacGroupLabel(text: "Auto-recap")
            MacTile {
                MacRow(
                    "Long dictation auto-recap",
                    description: "When a single dictation runs longer than the threshold, ask the LLM for a quick bullet recap and append it to the History row. 0 disables.",
                    showsDivider: false
                ) {
                    HStack(spacing: 6) {
                        Stepper(value: Binding(
                            get: { Int(appState.autoRecapThresholdSecs) },
                            set: { appState.autoRecapThresholdSecs = UInt32(max(0, $0))
                                   DimmyCore.shared.setConfig(appState.toRustConfig()) }
                        ), in: 0...3600, step: 30) {
                            Text(appState.autoRecapThresholdSecs == 0
                                 ? "Off"
                                 : "\(appState.autoRecapThresholdSecs)s threshold")
                                .font(.system(size: 12))
                        }
                        .controlSize(.small)
                    }
                }
            }
            MacGroupFooter(text: "The recap is appended to the History row's enhanced_text — visible in the History detail under the original transcript.")
        }
    }

    private var appearanceGroup: some View {
        Group {
            MacGroupLabel(text: "Appearance")
            MacTile {
                MacRow(
                    "Show in Dock",
                    description: "When off, Dimmy is hidden from the Dock and Cmd-Tab"
                ) {
                    Toggle("", isOn: $appState.showInDock)
                        .toggleStyle(.switch)
                        .labelsHidden()
                }
                MacRow(
                    "Show in menu bar",
                    description: "When off, the icon at the top-right disappears. At least one of Dock or menu bar must stay on",
                    showsDivider: false
                ) {
                    Toggle("", isOn: $appState.showInMenuBar)
                        .toggleStyle(.switch)
                        .labelsHidden()
                }
            }
        }
    }

    private var performanceGroup: some View {
        Group {
            MacGroupLabel(text: "Performance")
            MacTile {
                MacRow(
                    "Metal acceleration",
                    description: "GPU-accelerated whisper.cpp + llama.cpp on Apple Silicon",
                    showsDivider: false
                ) {
                    Label("Active", systemImage: "checkmark.circle.fill")
                        .font(.system(size: 12, weight: .medium))
                        .foregroundStyle(.green)
                }
            }
        }
    }

    private var diagnosticsGroup: some View {
        Group {
            MacGroupLabel(text: "Diagnostics")
            MacTile {
                MacRow(
                    "LLM log enabled",
                    description: "Write all prompts and responses to disk for debugging"
                ) {
                    Toggle("", isOn: Binding(
                        get: { appState.llmLogEnabled },
                        set: { newValue in
                            appState.llmLogEnabled = newValue
                            DimmyCore.shared.setConfig(appState.toRustConfig())
                        }
                    ))
                    .toggleStyle(.switch)
                    .labelsHidden()
                }

                MacRow(
                    "Audio debug",
                    description: "Save raw audio of each recording locally",
                    showsDivider: false
                ) {
                    Toggle("", isOn: Binding(
                        get: { appState.audioDebugEnabled },
                        set: { newValue in
                            appState.audioDebugEnabled = newValue
                            DimmyCore.shared.setConfig(appState.toRustConfig())
                        }
                    ))
                    .toggleStyle(.switch)
                    .labelsHidden()
                }
            }
        }
    }

    private var resetGroup: some View {
        Group {
            MacGroupLabel(text: "Reset")
            MacTile {
                MacRow(
                    "Open log folder",
                    description: "View dimmy.log and audio_debug session dumps in Finder"
                ) {
                    Button {
                        if let logUrl = logDirectoryURL() {
                            NSWorkspace.shared.activateFileViewerSelecting([logUrl])
                        }
                    } label: {
                        Label("Reveal in Finder", systemImage: "folder.fill")
                    }
                    .controlSize(.small)
                }

                MacRow(
                    "Reset all settings",
                    description: "Restore Dimmy to factory defaults",
                    showsDivider: false
                ) {
                    Button(role: .destructive) {
                        // Phase 6: confirm dialog + DimmyCore.shared.resetConfig().
                        // Today this is intentionally a no-op so the button
                        // can ship as a visible-but-safe placeholder.
                    } label: {
                        Text("Reset…")
                            .foregroundStyle(.red)
                    }
                    .controlSize(.small)
                }
            }
        }
    }

    /// Path to the macOS log directory:
    /// `~/Library/Application Support/dimmy/` (prod) or
    /// `~/Library/Application Support/dimmy-staging/` (staging-flavor).
    /// Mirrors the Rust core's `config_dir_path()` via
    /// `DimmyCore.shared.configDirURL` so flavor selection stays in
    /// one place. Hardcoding "dimmy" here would point staging users
    /// at the prod log dir, missing their actual diagnostics.
    private func logDirectoryURL() -> URL? {
        guard let dir = DimmyCore.shared.configDirURL else { return nil }
        // Ensure it exists so the Finder doesn't pop a "doesn't exist" alert.
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir
    }
}

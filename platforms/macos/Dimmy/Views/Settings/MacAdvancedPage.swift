import AppKit
import SwiftUI

// Advanced, diagnostics + GPU acceleration. Visible only when the
// sidebar Advanced toggle is ON (gated by `MacSettingsTab.advanced`
// filter in MacSettingsContainerView.filteredTabs).

struct MacAdvancedPage: View {
    @ObservedObject var appState: AppState

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            MacNote(
                title: "Debug settings",
                message: "These options can affect performance and stability. Change them only if you know what you're doing.",
                systemImage: "exclamationmark.triangle.fill"
            )
            .padding(.bottom, 8)

            performanceGroup
            autoRecapGroup
            // Meetings storage folder moved to Output > Advanced
            // (settings-redesign-checklist.md says it lives on Output).
            #if DEBUG
            debugSeedGroup
            #endif
            diagnosticsGroup
            resetGroup
        }
    }

    // Meeting storage folder lives on MacOutputPage now (Advanced
    // gate). See settings-redesign-checklist.md "Meetings folder = A
    // on Output".

    #if DEBUG
    /// Debug-only: drop one sample row with a synthesised WAV so the
    /// History detail's waveform + Raw/Enhanced toggle can be demoed
    /// without a microphone. Compiled out in Release/Staging, under
    /// `#if DEBUG` instead of a runtime flag so a binary you ship
    /// physically cannot insert fake rows.
    private var debugSeedGroup: some View {
        Group {
            MacGroupLabel(text: "Debug · history seed")
            MacTile {
                MacRow(
                    "Insert sample row",
                    description: "Debug-only.",
                    hint: "Adds one fake history entry with a synthesised 4 s audio file so the waveform and playback UI can be exercised without making a real recording. Compiled out of Release builds.",
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

    /// Phase 6.4, fire-and-forget recap on long dictations. Independent
    /// of meeting mode (which always recaps) and from the dictation
    /// rewrite (which uses llm_style). 0 = disabled.
    private var autoRecapGroup: some View {
        Group {
            MacGroupLabel(text: "Auto-recap")
            MacTile {
                MacRow(
                    "Long dictation auto-recap",
                    description: "0 disables.",
                    hint: "When a single dictation runs longer than the threshold, Dimmy asks the LLM for a quick bullet recap and appends it to the History row below the original transcript.",
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
        }
    }

    private var performanceGroup: some View {
        Group {
            MacGroupLabel(text: "Performance")
            MacTile {
                MacRow(
                    "Metal acceleration",
                    hint: "Whisper.cpp and llama.cpp use the Apple Silicon GPU via Metal. Always on, no toggle. Listed here so you can confirm it's active in support tickets.",
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
                    hint: "Writes every LLM prompt and response to dimmy.log. Useful for debugging rewrite issues, leave off otherwise so the log doesn't fill with content."
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
                    hint: "Saves the raw mic capture and preprocessed buffer for every recording. Helpful when reporting an audio issue; consumes disk so leave off in normal use.",
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
                    hint: "Reveals dimmy.log and audio_debug session dumps in Finder. Attach these when filing a support ticket so we can diagnose without remote access."
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
                    hint: "Restores Dimmy to factory defaults. Wipes config.json (keeps your encrypted API keys, history, and saved meetings). Currently a placeholder, wire-up lands in a later phase.",
                    showsDivider: false
                ) {
                    Button(role: .destructive) {
                        // Phase 6: confirm dialog + DimmyCore.shared.resetConfig().
                        // Today this is intentionally a no-op so the button
                        // can ship as a visible-but-safe placeholder.
                    } label: {
                        Text("Reset...")
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

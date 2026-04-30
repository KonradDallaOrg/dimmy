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
            diagnosticsGroup
            resetGroup
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

    /// Path to the macOS log directory: `~/Library/Application Support/dimmy/`.
    /// Mirrors the Rust core's `config_dir_path()` on macOS.
    private func logDirectoryURL() -> URL? {
        guard let support = FileManager.default.urls(
            for: .applicationSupportDirectory, in: .userDomainMask
        ).first else { return nil }
        let dir = support.appendingPathComponent("dimmy", isDirectory: true)
        // Ensure it exists so the Finder doesn't pop a "doesn't exist" alert.
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir
    }
}

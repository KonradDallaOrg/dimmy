import SwiftUI

// Shortcut — hotkey display + push-to-talk vs toggle behaviour. Capture
// of new shortcuts reuses the legacy ShortcutSettingsView's recorder via
// a sheet because rebuilding the modifier-flag capture from scratch is
// orthogonal to the visual redesign.

struct MacShortcutPage: View {
    @ObservedObject var appState: AppState
    @State private var showRecorder = false

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            hotkeyGroup
            behaviorGroup
        }
        .sheet(isPresented: $showRecorder) {
            // Phase 4: dedicated capture sheet. For now jump back to the
            // legacy ShortcutSettingsView in a sheet so users can still
            // record a new combo without leaving the Tahoe Settings.
            // The legacy view has no Close button of its own — wrap it
            // with a header so the sheet is dismissible.
            VStack(alignment: .leading, spacing: 0) {
                HStack {
                    Text("Change shortcut")
                        .font(.system(size: 15, weight: .semibold))
                    Spacer()
                    Button("Done") { showRecorder = false }
                        .keyboardShortcut(.defaultAction)
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 12)
                Divider()
                ShortcutSettingsView(appState: appState)
                    .padding(16)
            }
            .frame(minWidth: 480, minHeight: 360)
        }
    }

    private var hotkeyGroup: some View {
        Group {
            MacGroupLabel(text: "Hotkey")
            MacTile {
                MacRow(
                    "Activation hotkey",
                    description: "Click to capture a new key combination",
                    icon: "keyboard.fill",
                    iconBackground: Color(red: 0.04, green: 0.52, blue: 1.00)
                ) {
                    HStack(spacing: 4) {
                        ForEach(appState.shortcut.displayParts, id: \.self) { glyph in
                            MacKeycap(glyph: glyph)
                        }
                    }
                    Button("Change…") { showRecorder = true }
                        .controlSize(.small)
                }

                MacRow("Behavior", showsDivider: false) {
                    Picker("", selection: Binding(
                        get: { appState.preferredMode == .pushToTalk ? "ptt" : "toggle" },
                        set: { newValue in
                            appState.preferredMode = newValue == "ptt" ? .pushToTalk : .toggle
                            DimmyCore.shared.setConfig(appState.toRustConfig())
                        }
                    )) {
                        Text("Push-to-talk").tag("ptt")
                        Text("Toggle").tag("toggle")
                    }
                    .pickerStyle(.segmented)
                    .labelsHidden()
                    .frame(width: 200)
                }
            }
            MacGroupFooter(text: "Push-to-talk records while held; release to transcribe. Toggle starts and stops on each press.")
        }
    }

    private var behaviorGroup: some View {
        Group {
            MacGroupLabel(text: "Status")
            MacTile {
                MacRow(
                    "CGEventTap status",
                    description: "Required for the global shortcut to intercept keys system-wide",
                    showsDivider: false
                ) {
                    statusBadge
                }
            }
        }
    }

    @ViewBuilder
    private var statusBadge: some View {
        switch appState.hotkeyStatus {
        case .installed:
            Label("Active", systemImage: "checkmark.circle.fill")
                .foregroundStyle(.green)
                .font(.system(size: 12, weight: .medium))
        case .accessibilityMissing:
            Label("Accessibility required", systemImage: "exclamationmark.triangle.fill")
                .foregroundStyle(.orange)
                .font(.system(size: 12, weight: .medium))
        case .tapFailed:
            Label("Tap failed", systemImage: "xmark.octagon.fill")
                .foregroundStyle(.red)
                .font(.system(size: 12, weight: .medium))
        case .uninstalled:
            Label("Initialising…", systemImage: "hourglass")
                .foregroundStyle(.gray)
                .font(.system(size: 12, weight: .medium))
        }
    }
}

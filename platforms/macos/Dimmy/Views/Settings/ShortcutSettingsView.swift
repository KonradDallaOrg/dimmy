import SwiftUI

struct ShortcutSettingsView: View {
    @ObservedObject var appState: AppState
    @State private var showRecorder = false
    @State private var conflictError: String?

    /// Preset shortcuts the user can pick with a click
    private let presets: [(label: String, shortcut: ModifierShortcut)] = [
        ("⌃⌥", ModifierShortcut(fn: false, control: true, option: true, command: false, shift: false)),
        ("⌃⇧", ModifierShortcut(fn: false, control: true, option: false, command: false, shift: true)),
        ("⌥⇧", ModifierShortcut(fn: false, control: false, option: true, command: false, shift: true)),
        ("fn", ModifierShortcut.fnOnly),
    ]

    var body: some View {
        Form {
            Section("Dictation Shortcut") {
                HStack {
                    Text("Shortcut")
                    Spacer()

                    // Recording happens in the same sheet as the redesigned
                    // Settings and onboarding.
                    Button(action: { showRecorder = true }) {
                        HStack(spacing: 4) {
                            ForEach(appState.shortcut.displayParts, id: \.self) { part in
                                Text(part)
                                    .font(.system(size: 13, weight: .semibold, design: .rounded))
                                    .padding(.horizontal, 8)
                                    .padding(.vertical, 4)
                                    .background(
                                        RoundedRectangle(cornerRadius: 5)
                                            .fill(Color(nsColor: .controlBackgroundColor))
                                    )
                                    .overlay(
                                        RoundedRectangle(cornerRadius: 5)
                                            .stroke(Color.primary.opacity(0.15), lineWidth: 1)
                                    )
                            }
                        }
                    }
                    .buttonStyle(.plain)
                }

                // Preset shortcuts — clickable
                HStack(spacing: 8) {
                    Text("Presets:")
                        .font(.system(size: 11))
                        .foregroundColor(.secondary)
                    ForEach(presets, id: \.label) { preset in
                        Button(preset.label) {
                            commitShortcut(preset.shortcut)
                        }
                        .buttonStyle(.bordered)
                        .controlSize(.small)
                        .tint(appState.shortcut == preset.shortcut ? .accentColor : nil)
                    }
                }

                if let err = conflictError {
                    Label(err, systemImage: "exclamationmark.triangle.fill")
                        .font(.system(size: 11))
                        .foregroundStyle(.orange)
                }

                Text("Hold the shortcut to start dictating, release to paste")
                    .font(.system(size: 11))
                    .foregroundColor(.secondary)
            }

            Section("Mode") {
                Picker("Default mode", selection: $appState.preferredMode) {
                    ForEach(RecordingMode.allCases, id: \.self) { mode in
                        Text(mode.rawValue).tag(mode)
                    }
                }
                .pickerStyle(.segmented)
                .onChange(of: appState.preferredMode) { _, _ in syncShortcutToRust() }

                VStack(alignment: .leading, spacing: 6) {
                    Label("Push-to-talk: hold shortcut, release to paste", systemImage: "hand.raised")
                    Label("Toggle: double-tap to start, tap again to stop", systemImage: "arrow.triangle.2.circlepath")
                }
                .font(.system(size: 11))
                .foregroundColor(.secondary)
            }
        }
        .formStyle(.grouped)
        .sheet(isPresented: $showRecorder) {
            ShortcutRecorderSheet(appState: appState, target: .dictation, isPresented: $showRecorder)
        }
    }

    /// Apply the new dictation chord only if it doesn't collide with the
    /// command / dictionary / meeting hotkeys. Until 2026-07-03 this side
    /// committed unchecked — the command recorder validates against the
    /// dictation chord, but changing the DICTATION chord afterwards could
    /// silently create the same overlap (dictation fires first in
    /// HotkeyManager.handleFlagsAll, so an overlapped command chord
    /// degrades into plain dictation of the spoken instruction).
    private func commitShortcut(_ candidate: ModifierShortcut) {
        if let clash = conflictLabel(candidate) {
            conflictError = "This combo overlaps the \(clash). Pick another."
            return
        }
        conflictError = nil
        appState.shortcut = candidate
        syncShortcutToRust()
    }

    /// Human name of the colliding hotkey, nil when the candidate is free.
    /// fn-only chords have no Rust grammar (fn is not a std modifier) and
    /// are skipped — same inherent limit as the command recorder's check.
    private func conflictLabel(_ candidate: ModifierShortcut) -> String? {
        let grammar = candidate.rustGrammar
        guard !grammar.isEmpty else { return nil }
        if let cmd = appState.commandHotkey, !cmd.rustGrammar.isEmpty,
           DimmyCore.shared.hotkeyCombosConflict(grammar, cmd.rustGrammar) {
            return "Command Mode hotkey"
        }
        let dictGrammar = appState.dictHotkey.rustGrammar
        if !dictGrammar.isEmpty,
           DimmyCore.shared.hotkeyCombosConflict(grammar, dictGrammar) {
            return "Add-to-dictionary hotkey"
        }
        if let mtg = appState.meetingHotkey, !mtg.rustGrammar.isEmpty,
           DimmyCore.shared.hotkeyCombosConflict(grammar, mtg.rustGrammar) {
            return "Meeting hotkey"
        }
        return nil
    }

    private func syncShortcutToRust() {
        DimmyCore.shared.setConfig(appState.toRustConfig())
    }
}

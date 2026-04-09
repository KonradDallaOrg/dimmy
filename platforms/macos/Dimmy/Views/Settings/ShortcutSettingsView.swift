import SwiftUI

struct ShortcutSettingsView: View {
    @ObservedObject var appState: AppState
    @State private var isRecording = false
    @State private var localMonitor: Any?
    @State private var globalMonitor: Any?

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

                    if isRecording {
                        Text("Press keys...")
                            .font(.system(size: 13))
                            .foregroundColor(.orange)
                            .padding(.horizontal, 12)
                            .padding(.vertical, 6)
                            .background(
                                RoundedRectangle(cornerRadius: 6)
                                    .fill(Color.orange.opacity(0.1))
                            )
                    } else {
                        Button(action: { startRecording() }) {
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
                }

                // Preset shortcuts — clickable
                HStack(spacing: 8) {
                    Text("Presets:")
                        .font(.system(size: 11))
                        .foregroundColor(.secondary)
                    ForEach(presets, id: \.label) { preset in
                        Button(preset.label) {
                            appState.shortcut = preset.shortcut
                            syncShortcutToRust()
                        }
                        .buttonStyle(.bordered)
                        .controlSize(.small)
                        .tint(appState.shortcut == preset.shortcut ? .accentColor : nil)
                    }
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
        .onDisappear {
            stopRecording()
        }
    }

    private func startRecording() {
        isRecording = true
        localMonitor = NSEvent.addLocalMonitorForEvents(matching: .flagsChanged) { event in
            handleFlags(event.modifierFlags)
            return event
        }
        globalMonitor = NSEvent.addGlobalMonitorForEvents(matching: .flagsChanged) { event in
            Task { @MainActor in
                handleFlags(event.modifierFlags)
            }
        }
    }

    private func handleFlags(_ modifierFlags: NSEvent.ModifierFlags) {
        let flags = modifierFlags.intersection(.deviceIndependentFlagsMask)
        let candidate = ModifierShortcut(
            fn: flags.contains(.function),
            control: flags.contains(.control),
            option: flags.contains(.option),
            command: flags.contains(.command),
            shift: flags.contains(.shift)
        )
        if candidate.isValid {
            appState.shortcut = candidate
            syncShortcutToRust()
            stopRecording()
        }
    }

    private func stopRecording() {
        isRecording = false
        if let m = localMonitor { NSEvent.removeMonitor(m); localMonitor = nil }
        if let m = globalMonitor { NSEvent.removeMonitor(m); globalMonitor = nil }
    }

    private func syncShortcutToRust() {
        DimmyCore.shared.setConfig(appState.toRustConfig())
    }
}

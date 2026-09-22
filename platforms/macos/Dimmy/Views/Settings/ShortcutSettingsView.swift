import SwiftUI

struct ShortcutSettingsView: View {
    @ObservedObject var appState: AppState
    @State private var isRecording = false
    @State private var localMonitor: Any?
    @State private var globalMonitor: Any?
    @State private var keyMonitor: Any?
    /// Deferred commit of a modifier-only reading, cancelled when a key
    /// lands first. See `handleFlags`.
    @State private var pendingWork: DispatchWorkItem?
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
        // A chord with a key (⌃⇧D) starts life as the same modifier press
        // as a modifier-only one, so the keyDown decides which of the two
        // the user meant. Local-only, like the Command-Mode recorder: the
        // sheet has focus while recording.
        keyMonitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { event in
            handleKeyDown(event)
            return nil
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
        pendingWork?.cancel()
        guard candidate.isValid else { return }
        // Hold the modifier-only reading briefly: the user may be on their
        // way to ⌃⇧D, and committing ⌃⇧ the instant it is valid would stop
        // the recorder before the D ever arrives. Any keyDown in the
        // meantime cancels this and wins.
        let work = DispatchWorkItem {
            commitShortcut(candidate)
            stopRecording()
        }
        pendingWork = work
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.6, execute: work)
    }

    /// Modifier(s) + key. Mirrors the Command-Mode recorder: at least one
    /// modifier, and a key the Rust grammar knows, so the combo that gets
    /// persisted can always be handed to the conflict check.
    private func handleKeyDown(_ event: NSEvent) {
        pendingWork?.cancel()
        let flags = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
        let modCount = [
            flags.contains(.control), flags.contains(.option),
            flags.contains(.command), flags.contains(.shift),
        ].filter { $0 }.count
        guard modCount >= 1 else {
            conflictError = "Hold at least one modifier together with the key."
            return
        }
        guard let rustName = HotkeyCombo.macKeyCodeRustName(event.keyCode) else {
            conflictError = "Unsupported key. Try a letter, number, or function key."
            return
        }
        let display: String
        if let chars = event.charactersIgnoringModifiers, !chars.isEmpty,
           chars.first?.isLetter == true || chars.first?.isNumber == true {
            display = chars.uppercased()
        } else {
            display = rustName.uppercased()
        }
        let candidate = ModifierShortcut(
            fn: flags.contains(.function),
            control: flags.contains(.control),
            option: flags.contains(.option),
            command: flags.contains(.command),
            shift: flags.contains(.shift),
            keyCode: event.keyCode,
            keyChar: display
        )
        commitShortcut(candidate)
        stopRecording()
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

    private func stopRecording() {
        isRecording = false
        pendingWork?.cancel()
        pendingWork = nil
        if let m = localMonitor { NSEvent.removeMonitor(m); localMonitor = nil }
        if let m = globalMonitor { NSEvent.removeMonitor(m); globalMonitor = nil }
        if let m = keyMonitor { NSEvent.removeMonitor(m); keyMonitor = nil }
    }

    private func syncShortcutToRust() {
        DimmyCore.shared.setConfig(appState.toRustConfig())
    }
}

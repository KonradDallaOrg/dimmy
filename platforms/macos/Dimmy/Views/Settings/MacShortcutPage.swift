import SwiftUI

// Shortcut, hotkey display + push-to-talk vs toggle behaviour. Capture
// of new shortcuts reuses the legacy ShortcutSettingsView's recorder via
// a sheet because rebuilding the modifier-flag capture from scratch is
// orthogonal to the visual redesign.

struct MacShortcutPage: View {
    @ObservedObject var appState: AppState
    @State private var showRecorder = false
    @State private var showDictRecorder = false
    @State private var showCommandRecorder = false

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            hotkeyGroup
            // Add-to-dictionary shortcut is power-user fodder per
            // settings-redesign-checklist.md ("Add-to-dictionary
            // shortcut = A"). Gate behind Advanced so the Simple
            // Shortcut page stays focused on the activation hotkey.
            if appState.showAdvanced {
                dictHotkeyGroup
                commandHotkeyGroup
            }
            behaviorGroup
        }
        .sheet(isPresented: $showDictRecorder) {
            DictHotkeyRecorderSheet(appState: appState, isPresented: $showDictRecorder)
                .frame(minWidth: 420, minHeight: 220)
        }
        .sheet(isPresented: $showCommandRecorder) {
            CommandHotkeyRecorderSheet(appState: appState, isPresented: $showCommandRecorder)
                .frame(minWidth: 460, minHeight: 260)
        }
        .sheet(isPresented: $showRecorder) {
            // Phase 4: dedicated capture sheet. For now jump back to the
            // legacy ShortcutSettingsView in a sheet so users can still
            // record a new combo without leaving the Tahoe Settings.
            // The legacy view has no Close button of its own, wrap it
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
                    description: "Press Change... to capture a new combo.",
                    hint: "Press the keys you want, then release to confirm. This is the shortcut you press to dictate.",
                    hintURL: URL(string: "https://dimmy.app/help/hotkey-change"),
                    icon: "keyboard.fill",
                    iconBackground: Color(red: 0.04, green: 0.52, blue: 1.00)
                ) {
                    HStack(spacing: 4) {
                        ForEach(appState.shortcut.displayParts, id: \.self) { glyph in
                            MacKeycap(glyph: glyph)
                        }
                    }
                    Button("Change...") { showRecorder = true }
                        .controlSize(.small)
                }

                MacRow(
                    "Behavior",
                    hint: "Push-to-Talk records only while you hold the keys. Toggle starts on one press and stops on the next.",
                    hintURL: URL(string: "https://dimmy.app/help/hotkey-modes"),
                    showsDivider: false
                ) {
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
        }
    }

    private var dictHotkeyGroup: some View {
        Group {
            MacGroupLabel(text: "Dictionary")
            MacTile {
                MacRow(
                    "Add to dictionary",
                    description: "Select text in any app, then press the combo.",
                    hint: "Select text in any app and press these keys to add it to your custom dictionary. Press the keys here, then release to confirm.",
                    hintURL: URL(string: "https://dimmy.app/help/hotkey-change"),
                    icon: "text.badge.plus",
                    iconBackground: Color(red: 0.40, green: 0.73, blue: 0.42),
                    showsDivider: false
                ) {
                    HStack(spacing: 4) {
                        ForEach(appState.dictHotkey.displayParts, id: \.self) { glyph in
                            MacKeycap(glyph: glyph)
                        }
                    }
                    Button("Change...") { showDictRecorder = true }
                        .controlSize(.small)
                }
            }
        }
    }

    private var commandHotkeyGroup: some View {
        Group {
            MacGroupLabel(text: "Command shortcut")
            MacTile {
                MacRow(
                    "One-shot Command Mode",
                    description: appState.commandHotkey == nil
                        ? "Press once to dictate an instruction over your selection. No combo set."
                        : "Press once to dictate an instruction over your selection.",
                    hint: "Triggers Command Mode for the next dictation only. The pill goes amber while it's active; after the paste, the mode auto-clears. The sticky Command Mode toggle in the pill menu is independent.",
                    hintURL: URL(string: "https://dimmy.app/help/command-mode"),
                    icon: "wand.and.stars",
                    iconBackground: Color(red: 1.0, green: 0.62, blue: 0.04),
                    showsDivider: false
                ) {
                    if let combo = appState.commandHotkey {
                        HStack(spacing: 4) {
                            ForEach(combo.displayParts, id: \.self) { glyph in
                                MacKeycap(glyph: glyph)
                            }
                        }
                    } else {
                        Text("Not set")
                            .font(.system(size: 12))
                            .foregroundStyle(.secondary)
                    }
                    Button(appState.commandHotkey == nil ? "Set..." : "Change...") {
                        showCommandRecorder = true
                    }
                    .controlSize(.small)
                    if appState.commandHotkey != nil {
                        Button("Remove") {
                            appState.commandHotkey = nil
                        }
                        .controlSize(.small)
                    }
                }
            }
        }
    }

    private var behaviorGroup: some View {
        Group {
            MacGroupLabel(text: "Status")
            MacTile {
                MacRow(
                    "CGEventTap status",
                    hint: "The low-level event tap that lets Dimmy intercept your hotkey from any focused app. Requires Accessibility (and, for Fn-key combos, Input Monitoring), check Permissions if this stays orange.",
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
            Label("Initialising...", systemImage: "hourglass")
                .foregroundStyle(.gray)
                .font(.system(size: 12, weight: .medium))
        }
    }
}

/// Capture sheet for the "add to dictionary" hotkey. Press any
/// modifier+letter combo to bind. Mirrors the Win
/// `DictHotkeyCaptureDialog` semantics: at least one modifier required,
/// letter must be A-Z. The sheet uses `NSEvent.addLocalMonitorForEvents`
/// while shown so we don't fight with the global CGEventTap; closes
/// itself after a valid bind. The persistence side (UserDefaults +
/// DictHotkeyManager refresh) is driven by `AppState.dictHotkey.didSet`.
private struct DictHotkeyRecorderSheet: View {
    @ObservedObject var appState: AppState
    @Binding var isPresented: Bool
    @State private var monitor: Any?
    @State private var lastError: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack {
                Text("Capture dictionary hotkey")
                    .font(.system(size: 15, weight: .semibold))
                Spacer()
                Button("Cancel") { isPresented = false }
                    .keyboardShortcut(.cancelAction)
            }

            Text("Press a combination, at least one modifier plus a letter. Cmd+Shift+D is the default.")
                .font(.system(size: 12))
                .foregroundStyle(Color.macTextSecondary)
                .fixedSize(horizontal: false, vertical: true)

            HStack(spacing: 6) {
                ForEach(appState.dictHotkey.displayParts, id: \.self) { glyph in
                    MacKeycap(glyph: glyph)
                }
            }
            .frame(maxWidth: .infinity, minHeight: 56)
            .padding(.vertical, 12)
            .background(
                RoundedRectangle(cornerRadius: 10, style: .continuous)
                    .fill(Color(nsColor: .controlBackgroundColor))
            )
            .overlay(
                RoundedRectangle(cornerRadius: 10, style: .continuous)
                    .stroke(Color.macControlStroke, lineWidth: 0.5)
            )

            if let err = lastError {
                Label(err, systemImage: "exclamationmark.triangle.fill")
                    .font(.system(size: 11))
                    .foregroundStyle(.orange)
            } else {
                Text("Listening...")
                    .font(.system(size: 11))
                    .foregroundStyle(Color.macTextSecondary)
            }

            Spacer()

            HStack {
                Button("Restore default") {
                    appState.dictHotkey = .defaultDictHotkey
                }
                .controlSize(.small)
                Spacer()
                Button("Done") { isPresented = false }
                    .keyboardShortcut(.defaultAction)
            }
        }
        .padding(20)
        .onAppear { installMonitor() }
        .onDisappear { removeMonitor() }
    }

    private func installMonitor() {
        // Local key monitor, fires for keys delivered to this window.
        // We must consume the event (return nil) so the standard "key
        // beep on unhandled keyDown" doesn't fire for every press.
        // Explicit `-> NSEvent?` annotation, without it the compiler
        // sometimes infers `-> NSEvent` from a non-optional code path
        // (depending on Swift version) and refuses the `return nil`s.
        monitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { (event: NSEvent) -> NSEvent? in
            let flags = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
            let modCount = [
                flags.contains(.control), flags.contains(.option),
                flags.contains(.command), flags.contains(.shift),
            ].filter { $0 }.count
            if modCount == 0 {
                lastError = "Add at least one modifier"
                return nil
            }
            guard let chars = event.charactersIgnoringModifiers?.uppercased(),
                  let letter = chars.first, letter.isLetter else {
                lastError = "Use a letter (A-Z)"
                return nil
            }
            appState.dictHotkey = HotkeyCombo(
                control: flags.contains(.control),
                option: flags.contains(.option),
                command: flags.contains(.command),
                shift: flags.contains(.shift),
                keyCode: event.keyCode,
                keyChar: String(letter)
            )
            lastError = nil
            return nil
        }
    }

    private func removeMonitor() {
        if let m = monitor { NSEvent.removeMonitor(m) }
        monitor = nil
    }
}

/// Capture sheet for the dedicated one-shot Command-Mode hotkey.
/// Validation matches `DictHotkeyRecorderSheet` (≥1 modifier + A-Z
/// letter), with one extra check: the captured combo must not collide
/// with the dictation hotkey or the Add-to-dictionary hotkey. Mac mirror
/// of the conflict guard the user's spec called out — if a conflict is
/// detected we keep `lastError` set and refuse the bind so the user
/// can pick again without leaving the sheet.
private struct CommandHotkeyRecorderSheet: View {
    @ObservedObject var appState: AppState
    @Binding var isPresented: Bool
    @State private var monitor: Any?
    @State private var lastError: String?
    @State private var captured: HotkeyCombo?

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack {
                Text("Capture command hotkey")
                    .font(.system(size: 15, weight: .semibold))
                Spacer()
                Button("Cancel") { isPresented = false }
                    .keyboardShortcut(.cancelAction)
            }

            Text("Press a combination, at least one modifier plus a letter. This hotkey triggers Command Mode for the NEXT dictation only.")
                .font(.system(size: 12))
                .foregroundStyle(Color.macTextSecondary)
                .fixedSize(horizontal: false, vertical: true)

            HStack(spacing: 6) {
                let parts = captured?.displayParts ?? appState.commandHotkey?.displayParts ?? []
                if parts.isEmpty {
                    Text("Listening...")
                        .font(.system(size: 12))
                        .foregroundStyle(Color.macTextSecondary)
                } else {
                    ForEach(parts, id: \.self) { glyph in
                        MacKeycap(glyph: glyph)
                    }
                }
            }
            .frame(maxWidth: .infinity, minHeight: 56)
            .padding(.vertical, 12)
            .background(
                RoundedRectangle(cornerRadius: 10, style: .continuous)
                    .fill(Color(nsColor: .controlBackgroundColor))
            )
            .overlay(
                RoundedRectangle(cornerRadius: 10, style: .continuous)
                    .stroke(Color.macControlStroke, lineWidth: 0.5)
            )

            if let err = lastError {
                Label(err, systemImage: "exclamationmark.triangle.fill")
                    .font(.system(size: 11))
                    .foregroundStyle(.orange)
            } else if captured != nil {
                Label("Looks good. Press Done to confirm.", systemImage: "checkmark.circle.fill")
                    .font(.system(size: 11))
                    .foregroundStyle(.green)
            }

            Spacer()

            HStack {
                Spacer()
                Button("Done") {
                    if let combo = captured {
                        appState.commandHotkey = combo
                    }
                    isPresented = false
                }
                .keyboardShortcut(.defaultAction)
                .disabled(captured == nil && appState.commandHotkey == nil)
            }
        }
        .padding(20)
        .onAppear { installMonitor() }
        .onDisappear { removeMonitor() }
    }

    private func installMonitor() {
        monitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { (event: NSEvent) -> NSEvent? in
            let flags = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
            let modCount = [
                flags.contains(.control), flags.contains(.option),
                flags.contains(.command), flags.contains(.shift),
            ].filter { $0 }.count
            if modCount == 0 {
                lastError = "Add at least one modifier"
                captured = nil
                return nil
            }
            guard let chars = event.charactersIgnoringModifiers?.uppercased(),
                  let letter = chars.first, letter.isLetter else {
                lastError = "Use a letter (A-Z)"
                captured = nil
                return nil
            }
            let combo = HotkeyCombo(
                control: flags.contains(.control),
                option: flags.contains(.option),
                command: flags.contains(.command),
                shift: flags.contains(.shift),
                keyCode: event.keyCode,
                keyChar: String(letter)
            )
            // Conflict detection: the dedicated command hotkey must not
            // collide with the Add-to-dictionary hotkey. The main
            // dictation hotkey is modifier-only so a key-bearing combo
            // can never collide with it. Win parity: the recorder
            // rejects collisions inline rather than silently saving.
            if combo == appState.dictHotkey {
                lastError = "This combo is already the Add-to-dictionary hotkey. Pick another."
                captured = nil
                return nil
            }
            lastError = nil
            captured = combo
            return nil
        }
    }

    private func removeMonitor() {
        if let m = monitor { NSEvent.removeMonitor(m) }
        monitor = nil
    }
}

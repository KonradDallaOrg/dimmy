import SwiftUI

// Shortcut page: each shortcut as keys plus "Change...", and push-to-talk vs
// toggle. Recording a new shortcut happens in `ShortcutRecorderSheet`, the one
// sheet every place uses (this page, the legacy page, onboarding, wizards).

struct MacShortcutPage: View {
    @ObservedObject var appState: AppState
    @State private var editing: ShortcutTarget?

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
                meetingHotkeyGroup
            }
            behaviorGroup
        }
        .sheet(item: $editing) { target in
            ShortcutRecorderSheet(
                appState: appState,
                target: target,
                isPresented: Binding(get: { editing != nil }, set: { if !$0 { editing = nil } })
            )
        }
    }

    /// Keys, or "Not set", then the button that opens the sheet.
    @ViewBuilder
    private func shortcutControl(_ parts: [String], target: ShortcutTarget) -> some View {
        if parts.isEmpty {
            Text("Not set")
                .font(.system(size: 12))
                .foregroundStyle(.secondary)
        } else {
            HStack(spacing: 4) {
                ForEach(parts, id: \.self) { glyph in
                    MacKeycap(glyph: glyph)
                }
            }
        }
        Button(parts.isEmpty ? "Set..." : "Change...") { editing = target }
            .controlSize(.small)
    }

    private var hotkeyGroup: some View {
        Group {
            MacGroupLabel(text: "Hotkey")
            MacTile {
                MacRow(
                    "Activation hotkey",
                    description: "Press Change... to record a new combo.",
                    hint: "Click Change..., press the keys you want, then Save. This is the shortcut you press to dictate.",
                    hintURL: URL(string: "https://dimmy.app/help/hotkey-change"),
                    icon: "keyboard.fill",
                    iconBackground: Color(red: 0.04, green: 0.52, blue: 1.00)
                ) {
                    shortcutControl(appState.shortcut.displayParts, target: .dictation)
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
                    hint: "Select text in any app and press these keys to add it to your custom dictionary.",
                    hintURL: URL(string: "https://dimmy.app/help/hotkey-change"),
                    icon: "text.badge.plus",
                    iconBackground: Color(red: 0.40, green: 0.73, blue: 0.42),
                    showsDivider: false
                ) {
                    shortcutControl(appState.dictHotkey.displayParts, target: .dictionary)
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
                    shortcutControl(appState.commandHotkey?.displayParts ?? [], target: .command)
                }
            }
        }
    }

    private var meetingHotkeyGroup: some View {
        Group {
            MacGroupLabel(text: "Meeting shortcut")
            MacTile {
                MacRow(
                    "Start / stop meeting recording",
                    description: appState.meetingHotkey == nil
                        ? "Press to start a meeting recording; press again to stop and generate the recap. No combo set."
                        : "Press to start a meeting recording; press again to stop and generate the recap.",
                    hint: "Toggle-only (a meeting can't be push-to-talk). On start it shows the recording-consent prompt, then records in the background. The Meeting window does not open. Leave empty to avoid clashing with app shortcuts used during calls (⌘⇧M is Teams mute).",
                    hintURL: URL(string: "https://dimmy.app/help/meetings"),
                    icon: "person.2.wave.2.fill",
                    iconBackground: Color(red: 0.18, green: 0.81, blue: 0.56),
                    showsDivider: false
                ) {
                    shortcutControl(appState.meetingHotkey?.displayParts ?? [], target: .meeting)
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

/// Which shortcut a recorder sheet edits. The raw value is the core's target id.
enum ShortcutTarget: String, Identifiable {
    case dictation, command, meeting, dictionary

    var id: String { rawValue }

    var title: String {
        switch self {
        case .dictation: return "Dictation shortcut"
        case .command: return "Command shortcut"
        case .meeting: return "Meeting shortcut"
        case .dictionary: return "Add-to-dictionary shortcut"
        }
    }

    var hint: String {
        switch self {
        case .dictionary:
            return "Press the keys you want to use: a modifier and a key, like ⌘⇧D."
        case .dictation:
            return "Press the keys you want to use: a modifier and a key, two modifiers together like ⌃⌥, or fn."
        case .command, .meeting:
            return "Press the keys you want to use: a modifier and a key, or two modifiers together like ⌃⌥."
        }
    }
}

/// The one sheet that records a shortcut, for all four of them. The rules
/// (what is allowed, conflicts with the other shortcuts, shortcuts macOS
/// already uses) live in the core recorder, shared with Windows: this view
/// feeds it the keys and draws what comes back, one `hotkey_recorder` event
/// per key-down. The event taps step aside while it's open
/// (`dimmy_hotkey_recorder_is_active`), so the current shortcut can't fire.
struct ShortcutRecorderSheet: View {
    @ObservedObject var appState: AppState
    let target: ShortcutTarget
    @Binding var isPresented: Bool

    @State private var keys: [String] = []
    @State private var error = ""
    @State private var warnings: [String] = []
    @State private var canSave = false
    @State private var last: [String: Any] = [:]
    @State private var keyMonitor: Any?
    @State private var flagsMonitor: Any?

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text(target.title)
                .font(.system(size: 15, weight: .semibold))
            Text(target.hint)
                .font(.system(size: 12))
                .foregroundStyle(Color.macTextSecondary)
                .fixedSize(horizontal: false, vertical: true)

            keyArea

            HStack(spacing: 18) {
                if let label = resetLabel {
                    Button(action: reset) {
                        Label(label, systemImage: "arrow.counterclockwise")
                    }
                    .buttonStyle(.link)
                }
                if canRemove {
                    Button(action: remove) {
                        Label("Remove shortcut", systemImage: "trash")
                    }
                    .buttonStyle(.link)
                }
            }
            .font(.system(size: 12))
            .frame(maxWidth: .infinity)

            if !error.isEmpty {
                Label(error, systemImage: "xmark.octagon.fill")
                    .font(.system(size: 12))
                    .foregroundStyle(.red)
                    .fixedSize(horizontal: false, vertical: true)
            }
            ForEach(warnings, id: \.self) { warning in
                Label(warning, systemImage: "exclamationmark.triangle.fill")
                    .font(.system(size: 12))
                    .foregroundStyle(.orange)
                    .fixedSize(horizontal: false, vertical: true)
            }

            Text("Tab, Return and Esc on their own still work in this window.")
                .font(.system(size: 11))
                .foregroundStyle(Color.macTextSecondary)

            Spacer(minLength: 0)

            HStack {
                Spacer()
                Button("Cancel") { isPresented = false }
                    .keyboardShortcut(.cancelAction)
                Button("Save", action: save)
                    .keyboardShortcut(.defaultAction)
                    .disabled(!canSave)
            }
        }
        .padding(20)
        .frame(minWidth: 480, minHeight: 320)
        .onAppear(perform: begin)
        .onDisappear(perform: end)
        .onReceive(NotificationCenter.default.publisher(for: .dimmyHotkeyRecorder)) { note in
            apply(note.userInfo)
        }
    }

    /// The keys pop in as they go down.
    private var keyArea: some View {
        HStack(spacing: 8) {
            if keys.isEmpty {
                Text("Press a shortcut")
                    .font(.system(size: 12))
                    .foregroundStyle(Color.macTextSecondary)
            }
            ForEach(keys, id: \.self) { glyph in
                RecorderKeycap(glyph: glyph, isError: !error.isEmpty)
                    .transition(.scale(scale: 0.6).combined(with: .opacity))
            }
        }
        .animation(.spring(response: 0.28, dampingFraction: 0.72), value: keys)
        .animation(.easeInOut(duration: 0.15), value: error)
        .frame(maxWidth: .infinity, minHeight: 88)
        .background(
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .fill(Color(nsColor: .controlBackgroundColor))
        )
        .overlay(
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .stroke(Color.macControlStroke, lineWidth: 0.5)
        )
    }

    // MARK: - Current values

    private var currentParts: [String] {
        switch target {
        case .dictation: return appState.shortcut.displayParts
        case .command: return appState.commandHotkey?.displayParts ?? []
        case .meeting: return appState.meetingHotkey?.displayParts ?? []
        case .dictionary: return appState.dictHotkey.displayParts
        }
    }

    private var currentShortcuts: [String: String] {
        [
            "dictation": appState.shortcut.rustGrammar,
            "command": appState.commandHotkey?.rustGrammar ?? "",
            "meeting": appState.meetingHotkey?.rustGrammar ?? "",
            "dictionary": appState.dictHotkey.rustGrammar,
        ]
    }

    private var resetLabel: String? {
        switch target {
        case .dictation: return "Reset to \(ModifierShortcut.default.displayString)"
        case .dictionary: return "Reset to \(HotkeyCombo.defaultDictHotkey.displayString)"
        case .command, .meeting: return nil
        }
    }

    private var canRemove: Bool {
        switch target {
        case .command: return appState.commandHotkey != nil
        case .meeting: return appState.meetingHotkey != nil
        case .dictation, .dictionary: return false
        }
    }

    // MARK: - Recorder

    private func begin() {
        keys = currentParts
        let rc = DimmyCore.shared.recorderStart(target: target.rawValue, current: currentShortcuts)
        if rc != 0 { NSLog("[Shortcut] recorder_start(\(target.rawValue)) rc=\(rc)") }
        keyMonitor = NSEvent.addLocalMonitorForEvents(matching: [.keyDown, .keyUp]) { (event: NSEvent) -> NSEvent? in
            DimmyCore.shared.recorderKey(code: event.keyCode, down: event.type == .keyDown) ? nil : event
        }
        // Modifiers arrive as flag changes; the sheet still gets them.
        flagsMonitor = NSEvent.addLocalMonitorForEvents(matching: .flagsChanged) { (event: NSEvent) -> NSEvent? in
            if let down = Self.modifierIsDown(event) {
                _ = DimmyCore.shared.recorderKey(code: event.keyCode, down: down)
            }
            return event
        }
    }

    private func end() {
        DimmyCore.shared.recorderStop()
        if let m = keyMonitor { NSEvent.removeMonitor(m) }
        if let m = flagsMonitor { NSEvent.removeMonitor(m) }
        keyMonitor = nil
        flagsMonitor = nil
    }

    /// Whether the modifier this flagsChanged is about is now down.
    private static func modifierIsDown(_ event: NSEvent) -> Bool? {
        let flags = event.modifierFlags
        switch event.keyCode {
        case 0x37, 0x36: return flags.contains(.command)
        case 0x3A, 0x3D: return flags.contains(.option)
        case 0x3B, 0x3E: return flags.contains(.control)
        case 0x38, 0x3C: return flags.contains(.shift)
        case 0x3F: return flags.contains(.function)
        default: return nil
        }
    }

    private func apply(_ info: [AnyHashable: Any]?) {
        guard let payload = info as? [String: Any],
              payload["target"] as? String == target.rawValue else { return }
        keys = payload["keys"] as? [String] ?? []
        error = payload["error"] as? String ?? ""
        warnings = payload["warnings"] as? [String] ?? []
        canSave = payload["can_save"] as? Bool ?? false
        last = payload
    }

    // MARK: - Actions

    private func save() {
        guard canSave, let mods = last["mods"] as? [String: Bool] else { return }
        let keyName = last["key"] as? String ?? ""
        let native = (last["native_key"] as? Int).map { UInt16(truncatingIfNeeded: $0) } ?? 0
        let keyCode: UInt16? = keyName.isEmpty ? nil : native
        let keyChar = keyName.uppercased()
        let control = mods["ctrl"] ?? false
        let option = mods["alt"] ?? false
        let command = mods["win"] ?? false
        let shift = mods["shift"] ?? false
        let combo = HotkeyCombo(control: control, option: option, command: command, shift: shift,
                                keyCode: keyCode, keyChar: keyChar)
        switch target {
        case .dictation:
            appState.shortcut = ModifierShortcut(
                fn: mods["fn"] ?? false, control: control, option: option,
                command: command, shift: shift, keyCode: keyCode, keyChar: keyChar)
            DimmyCore.shared.setConfig(appState.toRustConfig())
        case .dictionary:
            guard keyCode != nil else { return }
            appState.dictHotkey = combo
        case .command:
            appState.commandHotkey = combo
        case .meeting:
            appState.meetingHotkey = combo
        }
        isPresented = false
    }

    private func reset() {
        switch target {
        case .dictation:
            appState.shortcut = .default
            DimmyCore.shared.setConfig(appState.toRustConfig())
        case .dictionary:
            appState.dictHotkey = .defaultDictHotkey
        case .command, .meeting:
            return
        }
        isPresented = false
    }

    private func remove() {
        switch target {
        case .command: appState.commandHotkey = nil
        case .meeting: appState.meetingHotkey = nil
        case .dictation, .dictionary: return
        }
        isPresented = false
    }
}

/// One key in the recorder sheet: accent when fine, red when the combination
/// can't be used. Accent fill with white text reads in both themes.
private struct RecorderKeycap: View {
    let glyph: String
    let isError: Bool

    var body: some View {
        Text(glyph)
            .font(.system(size: 18, weight: .semibold, design: .rounded))
            .foregroundStyle(isError ? Color.red : Color.white)
            .frame(minWidth: 44, minHeight: 44)
            .padding(.horizontal, 10)
            .background(
                RoundedRectangle(cornerRadius: 9, style: .continuous)
                    .fill(isError ? Color.red.opacity(0.15) : Color.accentColor)
            )
            .overlay(
                RoundedRectangle(cornerRadius: 9, style: .continuous)
                    .stroke(isError ? Color.red : Color.black.opacity(0.12), lineWidth: isError ? 1 : 0.5)
            )
    }
}

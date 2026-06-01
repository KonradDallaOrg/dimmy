import SwiftUI

struct ShortcutStepView: View {
    @ObservedObject var appState: AppState
    @State private var isRecording = false
    @State private var localMonitor: Any?
    @State private var globalMonitor: Any?

    private var activeShortcut: ModifierShortcut {
        appState.shortcut
    }

    private let presets: [(label: String, shortcut: ModifierShortcut)] = [
        ("fn", ModifierShortcut.fnOnly),
        ("⌃⌥", ModifierShortcut(fn: false, control: true, option: true, command: false, shift: false)),
        ("⌃⇧", ModifierShortcut(fn: false, control: true, option: false, command: false, shift: true)),
    ]

    var body: some View {
        ScrollView(.vertical, showsIndicators: false) {
            VStack(spacing: 18) {
                Text("Your Shortcut")
                    .font(.system(size: 26, weight: .bold))

                Text(modeBlurb)
                    .font(.system(size: 13))
                    .foregroundColor(.secondary)
                    .multilineTextAlignment(.center)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.horizontal, 32)

                currentShortcutDisplay

                HStack(spacing: 10) {
                    ForEach(presets, id: \.label) { preset in
                        presetButton(label: preset.label, shortcut: preset.shortcut)
                    }
                    customButton
                }
                .padding(.horizontal, 20)

                modeSelector

                if activeShortcut.isFnOnly {
                    fnConflictBanner
                        .padding(.horizontal, 20)
                }

                Spacer().frame(height: 8)
            }
            .padding(.horizontal, 32)
            .padding(.vertical, 18)
        }
        .onDisappear { stopRecording() }
    }

    private var modeBlurb: String {
        appState.preferredMode == .pushToTalk
            ? "Hold to dictate, release to transcribe and paste"
            : "Press to start, press again to transcribe and paste"
    }

    /// Push-to-talk vs Toggle selector. Mirrors the segmented control in
    /// Settings → Shortcut so first-launch users can pick their preferred
    /// mode before they ever open Settings.
    private var modeSelector: some View {
        VStack(spacing: 6) {
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
            .frame(width: 260)

            Text("You can change this any time in Settings → Shortcut.")
                .font(.system(size: 11))
                .foregroundColor(.secondary)
        }
        .padding(.top, 4)
    }

    private var currentShortcutDisplay: some View {
        HStack(spacing: 8) {
            if isRecording {
                Text("Press your shortcut...")
                    .font(.system(size: 15, weight: .medium))
                    .foregroundColor(.orange)
            } else {
                ForEach(Array(activeShortcut.displayParts.enumerated()), id: \.offset) { index, part in
                    if index > 0 {
                        Text("+")
                            .font(.system(size: 20, weight: .light))
                            .foregroundColor(.secondary)
                    }
                    Text(part)
                        .font(.system(size: 22, weight: .semibold, design: .rounded))
                        .padding(.horizontal, 16)
                        .padding(.vertical, 10)
                        .background(
                            RoundedRectangle(cornerRadius: 10)
                                .fill(Color(nsColor: .controlBackgroundColor))
                        )
                        .overlay(
                            RoundedRectangle(cornerRadius: 10)
                                .stroke(Color.primary.opacity(0.12), lineWidth: 1)
                        )
                }
            }
        }
        .frame(height: 52)
    }

    private func presetButton(label: String, shortcut: ModifierShortcut) -> some View {
        let isSelected = activeShortcut == shortcut
        return Button(action: {
            stopRecording()
            withAnimation(.easeInOut(duration: 0.2)) {
                appState.shortcut = shortcut
            }
        }) {
            Text(label)
                .font(.system(size: 15, weight: .medium, design: .rounded))
                .frame(maxWidth: .infinity)
                .padding(.vertical, 10)
                .background(
                    RoundedRectangle(cornerRadius: 8)
                        .fill(isSelected
                              ? Color.accentColor.opacity(0.15)
                              : Color(nsColor: .controlBackgroundColor))
                )
                .overlay(
                    RoundedRectangle(cornerRadius: 8)
                        .stroke(isSelected ? Color.accentColor : Color.primary.opacity(0.12),
                                lineWidth: isSelected ? 1.5 : 1)
                )
        }
        .buttonStyle(.plain)
    }

    private var customButton: some View {
        // "Custom" is selected whenever the active shortcut isn't one of the presets.
        let isCustomActive = !presets.contains(where: { $0.shortcut == activeShortcut })
        let isSelected = isRecording || isCustomActive
        return Button(action: {
            if isRecording {
                stopRecording()
            } else {
                startRecording()
            }
        }) {
            Text(isRecording ? "..." : "Custom")
                .font(.system(size: 13, weight: .medium))
                .frame(maxWidth: .infinity)
                .padding(.vertical, 10)
                .background(
                    RoundedRectangle(cornerRadius: 8)
                        .fill(isSelected
                              ? Color.accentColor.opacity(0.15)
                              : Color(nsColor: .controlBackgroundColor))
                )
                .overlay(
                    RoundedRectangle(cornerRadius: 8)
                        .stroke(isSelected ? Color.accentColor : Color.primary.opacity(0.12),
                                lineWidth: isSelected ? 1.5 : 1)
                )
        }
        .buttonStyle(.plain)
    }

    private var fnConflictBanner: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                Image(systemName: "exclamationmark.triangle.fill")
                    .foregroundColor(.orange)
                Text("macOS may intercept Fn")
                    .font(.system(size: 13, weight: .semibold))
            }
            Text("If Fn doesn't trigger Dimmy, open **System Settings → Keyboard → Dictation** and set **Shortcut** to anything else, or set **Press 🌐 Key to** to **Do Nothing**.")
                .font(.system(size: 12))
                .foregroundColor(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            Button("Open Keyboard Settings") {
                if let url = URL(string: "x-apple.systempreferences:com.apple.Keyboard-Settings.extension") {
                    NSWorkspace.shared.open(url)
                }
            }
            .buttonStyle(.link)
            .font(.system(size: 12))
        }
        .padding(12)
        .background(
            RoundedRectangle(cornerRadius: 10)
                .fill(Color.orange.opacity(0.08))
        )
        .overlay(
            RoundedRectangle(cornerRadius: 10)
                .stroke(Color.orange.opacity(0.3), lineWidth: 1)
        )
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
            withAnimation(.easeInOut(duration: 0.2)) {
                appState.shortcut = candidate
            }
            stopRecording()
        }
    }

    private func stopRecording() {
        isRecording = false
        if let m = localMonitor { NSEvent.removeMonitor(m); localMonitor = nil }
        if let m = globalMonitor { NSEvent.removeMonitor(m); globalMonitor = nil }
    }
}

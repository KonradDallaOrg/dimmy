import SwiftUI

struct ShortcutStepView: View {
    @ObservedObject var appState: AppState
    let onContinue: () -> Void
    @State private var isRecording = false
    @State private var pendingShortcut: ModifierShortcut?
    @State private var localMonitor: Any?
    @State private var globalMonitor: Any?

    private var activeShortcut: ModifierShortcut {
        pendingShortcut ?? appState.shortcut
    }

    /// Preset shortcuts the user can pick with a click (no keyboard needed)
    private let presets: [(label: String, shortcut: ModifierShortcut)] = [
        ("⌃⌥", ModifierShortcut(fn: false, control: true, option: true, command: false, shift: false)),
        ("⌃⇧", ModifierShortcut(fn: false, control: true, option: false, command: false, shift: true)),
        ("⌥⇧", ModifierShortcut(fn: false, control: false, option: true, command: false, shift: true)),
        ("fn", ModifierShortcut.fnOnly),
    ]

    var body: some View {
        ScrollView(.vertical, showsIndicators: false) {
        VStack(spacing: 20) {
            Text("Your Shortcut")
                .font(.system(size: 26, weight: .bold))

            Text("Hold to dictate, release to paste")
                .font(.system(size: 13))
                .foregroundColor(.secondary)

            // Current shortcut display / recorder
            Button(action: {
                if isRecording {
                    stopRecording()
                } else {
                    startRecording()
                }
            }) {
                VStack(spacing: 12) {
                    if isRecording {
                        Text("Press your shortcut...")
                            .font(.system(size: 15, weight: .medium))
                            .foregroundColor(.orange)
                            .frame(height: 50)
                    } else {
                        HStack(spacing: 10) {
                            ForEach(Array(activeShortcut.displayParts.enumerated()), id: \.offset) { index, part in
                                if index > 0 {
                                    Text("+")
                                        .font(.system(size: 22, weight: .light))
                                        .foregroundColor(.secondary)
                                }
                                Text(part)
                                    .font(.system(size: 24, weight: .semibold, design: .rounded))
                                    .padding(.horizontal, 18)
                                    .padding(.vertical, 12)
                                    .background(
                                        RoundedRectangle(cornerRadius: 10)
                                            .fill(Color(nsColor: .controlBackgroundColor))
                                            .shadow(color: .black.opacity(0.2), radius: 2, y: 1)
                                    )
                                    .overlay(
                                        RoundedRectangle(cornerRadius: 10)
                                            .stroke(Color.primary.opacity(0.15), lineWidth: 1)
                                    )
                            }
                        }
                        .frame(height: 50)
                    }

                    Text(isRecording ? "Hold 2+ modifier keys together" : "Click to record, or pick a preset below")
                        .font(.system(size: 12))
                        .foregroundColor(Color(nsColor: .tertiaryLabelColor))
                }
                .frame(maxWidth: .infinity)
                .padding(.vertical, 16)
                .background(
                    RoundedRectangle(cornerRadius: 14)
                        .fill(isRecording
                              ? Color.orange.opacity(0.08)
                              : Color(nsColor: .controlBackgroundColor).opacity(0.5))
                )
                .overlay(
                    RoundedRectangle(cornerRadius: 14)
                        .stroke(isRecording ? Color.orange.opacity(0.4) : Color.clear, lineWidth: 1.5)
                )
            }
            .buttonStyle(.plain)
            .padding(.horizontal, 10)

            // Preset shortcuts — clickable, no keyboard needed
            VStack(spacing: 8) {
                Text("Or pick a preset:")
                    .font(.system(size: 12))
                    .foregroundColor(.secondary)

                HStack(spacing: 12) {
                    ForEach(presets, id: \.label) { preset in
                        Button(action: {
                            withAnimation(.easeInOut(duration: 0.2)) {
                                pendingShortcut = preset.shortcut
                            }
                        }) {
                            Text(preset.label)
                                .font(.system(size: 16, weight: .medium, design: .rounded))
                                .padding(.horizontal, 14)
                                .padding(.vertical, 8)
                                .background(
                                    RoundedRectangle(cornerRadius: 8)
                                        .fill(activeShortcut == preset.shortcut
                                              ? Color.accentColor.opacity(0.15)
                                              : Color(nsColor: .controlBackgroundColor))
                                )
                                .overlay(
                                    RoundedRectangle(cornerRadius: 8)
                                        .stroke(activeShortcut == preset.shortcut
                                                ? Color.accentColor
                                                : Color.primary.opacity(0.12),
                                                lineWidth: activeShortcut == preset.shortcut ? 1.5 : 1)
                                )
                        }
                        .buttonStyle(.plain)
                    }
                }
            }

            if activeShortcut.isFnOnly {
                fnConflictBanner
                    .padding(.horizontal, 20)
            }

            Button(action: {
                if let pending = pendingShortcut {
                    appState.shortcut = pending
                }
                onContinue()
            }) {
                Text("Continue")
                    .font(.system(size: 15, weight: .semibold))
                    .frame(maxWidth: 220)
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.large)
            .padding(.top, 4)

            Spacer().frame(height: 12)
        }
        .padding(.horizontal, 32)
        .padding(.vertical, 20)
        }
        .onDisappear {
            stopRecording()
        }
    }

    /// Warn the user about macOS's "Press Fn key to …" setting. macOS consumes Fn for
    /// Dictation / Siri / Emoji picker if that setting is anything other than "Do Nothing",
    /// and there's no public API for us to override it.
    private var fnConflictBanner: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                Image(systemName: "exclamationmark.triangle.fill")
                    .foregroundColor(.orange)
                Text("macOS may intercept Fn")
                    .font(.system(size: 13, weight: .semibold))
            }
            Text("If Fn doesn't trigger Dimmy, open **System Settings → Keyboard → Dictation** and set **Shortcut** to anything else — or set **Press 🌐 Key to** to **Do Nothing**.")
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
        // Use BOTH local and global monitors — local works when window is key,
        // global works when using Accessibility Keyboard or other input methods
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
                pendingShortcut = candidate
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

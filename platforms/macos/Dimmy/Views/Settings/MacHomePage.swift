import SwiftUI

// Home, landing tab. Three blocks:
//   1. Hero (welcome + shortcut hint + Test microphone / Change shortcut buttons)
//   2. Stats grid (words / speaking time / time saved)
//   3. "Current setup" tile (STT provider, output style, shortcut summary)
//
// The "Recent dictations" tile from the design is omitted for now, the
// macOS app already has a full History tab, and the Home recent list
// would duplicate that data. We can add a 3-row preview here in Phase 6
// polish if the user wants it.

struct MacHomePage: View {
    @ObservedObject var appState: AppState
    let onTabChange: (MacSettingsTab) -> Void

    @State private var micTestRunning = false
    @State private var micTestResult: String? = nil  // nil = idle, "" = success
    @State private var micTestResetTask: Task<Void, Never>? = nil

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            hero
                .padding(.bottom, 8)

            statsGrid
                .padding(.bottom, 8)

            MacGroupLabel(text: "Transcribe a file")
            FileLoadCard(appState: appState)
                .padding(.bottom, 8)

            MacGroupLabel(text: "Meeting mode")
            MacTile {
                MacRow(
                    "Record a meeting",
                    description: "Live transcript and an AI recap when you stop.",
                    hint: "Records a long-form session independently of the dictation hotkey. Stop the meeting to get an LLM-generated recap with action items, saved alongside the transcript.",
                    hintURL: URL(string: "https://dimmy.app/help/use-meeting-recaps"),
                    icon: "rectangle.dashed.and.paperclip",
                    iconBackground: Color(red: 0.92, green: 0.25, blue: 0.48),
                    showsDivider: false
                ) {
                    Button {
                        AppDelegate.shared?.openMeetingWindow()
                    } label: {
                        Label("Open meeting", systemImage: "record.circle")
                    }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.small)
                }
            }
            .padding(.bottom, 8)

            MacGroupLabel(text: "Current setup")
            MacTile {
                MacRow(
                    "Speech-to-text",
                    description: sttDescription,
                    icon: "mic.fill",
                    iconBackground: Color(red: 1.00, green: 0.22, blue: 0.37)
                ) {
                    Button("Configure") { onTabChange(.voice) }
                        .controlSize(.small)
                }

                MacRow(
                    "Output style",
                    description: outputDescription,
                    icon: "lightbulb.fill",
                    iconBackground: Color(red: 1.00, green: 0.80, blue: 0.00)
                ) {
                    Circle()
                        .fill(MacStyleColor.color(for: appState.llmStyle))
                        .frame(width: 8, height: 8)
                        .shadow(color: MacStyleColor.color(for: appState.llmStyle)
                                    .opacity(0.5), radius: 4)
                    Button("Configure") { onTabChange(.output) }
                        .controlSize(.small)
                }

                MacRow(
                    "Shortcut",
                    description: appState.preferredMode == .pushToTalk
                                    ? "Push-to-talk" : "Toggle recording",
                    icon: "keyboard.fill",
                    iconBackground: Color(red: 0.04, green: 0.52, blue: 1.00),
                    showsDivider: false
                ) {
                    HStack(spacing: 4) {
                        ForEach(shortcutKeycaps, id: \.self) { glyph in
                            MacKeycap(glyph: glyph)
                        }
                    }
                }

                // "Appearance" row used to live here; it has moved
                // into the Advanced-gated "System" section below so
                // the Simple Home view stays focused on the four
                // navigation shortcuts (STT, Output style, Shortcut
                // + their current state).
                // Drop the last MacRow's divider for the new last row.
                let _ = ()
            }

            // SYSTEM section per docs/dev/settings-map.md "Vuoi" col:
            // Theme picker is Advanced-only on Mac. (Launch at login
            // sits in the macOS Login Items prefpane on this
            // platform; not duplicated as a Dimmy toggle yet.)
            if appState.showAdvanced {
                MacGroupLabel(text: "System")
                MacTile {
                    MacRow(
                        "Appearance",
                        description: "Dimmy follows the system theme by default.",
                        icon: "paintpalette.fill",
                        iconBackground: Color(red: 0.69, green: 0.32, blue: 0.87),
                        showsDivider: false
                    ) {
                        Picker("", selection: Binding(
                            get: { appState.theme },
                            set: { newValue in
                                appState.theme = newValue
                                applyTheme(newValue)
                            }
                        )) {
                            ForEach(AppTheme.allCases, id: \.self) { theme in
                                Text(theme.rawValue).tag(theme)
                            }
                        }
                        .labelsHidden()
                        .frame(width: 110)
                    }
                }
            }
        }
    }

    /// Theme is a UI-only preference (no Rust round-trip), directly flips
    /// the NSApp.appearance to follow / force light / force dark.
    private func applyTheme(_ theme: AppTheme) {
        switch theme {
        case .auto:  NSApp.appearance = nil
        case .light: NSApp.appearance = NSAppearance(named: .aqua)
        case .dark:  NSApp.appearance = NSAppearance(named: .darkAqua)
        }
    }

    // MARK: Hero

    private var hero: some View {
        MacHero(
            title: "Dimmy is ready to listen",
            subtitle: heroSubtitle,
            actions: AnyView(heroActions)
        ) {
            // Trailing visual: app icon. Falls back to a styled placeholder
            // when the asset isn't bundled. We reuse the existing asset
            // catalogue's `AppIcon` rather than loading a separate PNG.
            ZStack {
                RoundedRectangle(cornerRadius: 20, style: .continuous)
                    .fill(Color.primary.opacity(0.06))
                    .frame(width: 200, height: 96)
                Image("DimmyAppIcon")
                    .resizable()
                    .aspectRatio(contentMode: .fit)
                    .frame(width: 80, height: 80)
                    .shadow(color: Color.accentColor.opacity(0.35),
                            radius: 12, x: 0, y: 6)
            }
        }
    }

    private var heroActions: some View {
        HStack(spacing: 8) {
            Button {
                runMicTest()
            } label: {
                if micTestRunning {
                    HStack(spacing: 6) {
                        ProgressView().controlSize(.small).scaleEffect(0.7)
                        Text("Testing...")
                    }
                } else {
                    Label("Test microphone", systemImage: "mic.fill")
                }
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.regular)
            .disabled(micTestRunning)

            Button("Change shortcut...") { onTabChange(.shortcut) }
                .controlSize(.regular)

            if let result = micTestResult {
                Text(result.isEmpty ? "✓ Microphone OK" : result)
                    .font(.system(size: 11))
                    .foregroundStyle(result.isEmpty ? Color.green : Color.orange)
                    .lineLimit(2)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    /// Trigger the Rust core's audio probe. Blocking 1-2s call so we run
    /// it on a background queue. The result string is nil while idle,
    /// `""` after a successful probe, or an error message on failure.
    private func runMicTest() {
        guard !micTestRunning, DimmyCore.shared.isInitialized else { return }
        micTestRunning = true
        micTestResult = nil
        micTestResetTask?.cancel()
        DispatchQueue.global(qos: .userInitiated).async {
            let report = DimmyCore.shared.checkAudioHealth()
            DispatchQueue.main.async {
                micTestRunning = false
                if let report {
                    let ok = (report["ok"] as? Bool) ?? false
                    if ok {
                        micTestResult = ""
                    } else {
                        let err = (report["error"] as? String) ?? "Microphone unavailable"
                        micTestResult = err
                    }
                } else {
                    micTestResult = "Couldn't read mic status"
                }
                micTestResetTask = Task { @MainActor in
                    try? await Task.sleep(nanoseconds: 6_000_000_000)
                    if !Task.isCancelled { micTestResult = nil }
                }
            }
        }
    }

    private var heroSubtitle: String {
        let combo = appState.shortcut.displayString
        if combo.isEmpty {
            return "Press your shortcut anywhere to dictate, then release. Your voice never leaves the cloud provider you chose."
        }
        return "Hold \(combo) anywhere to dictate, then release. Your voice never leaves the cloud provider you chose."
    }

    private var shortcutKeycaps: [String] {
        appState.shortcut.displayParts
    }

    // MARK: Stats

    private var statsGrid: some View {
        HStack(spacing: 8) {
            MacStatTile(
                value: appState.statsTotalWords.formatted(),
                label: "Words dictated"
            )
            MacStatTile(
                value: speakingTimeText,
                label: "Speaking time"
            )
            MacStatTile(
                value: timeSavedText,
                label: "Time saved"
            )
        }
    }

    private var speakingTimeText: String {
        let secs = appState.statsTotalSpeakingSecs
        if secs < 1 { return "-" }
        let mins = Int(secs / 60)
        let remSecs = Int(secs.truncatingRemainder(dividingBy: 60))
        if mins == 0 { return "\(remSecs)s" }
        return String(format: "%d:%02d", mins, remSecs)
    }

    private var timeSavedText: String {
        // Same heuristic as Windows: typing about 40 WPM vs dictation about 150 WPM.
        let words = Double(appState.statsTotalWords)
        let secs = words * (1.0 / 40 - 1.0 / 150) * 60
        if secs < 1 { return "-" }
        if secs < 60 { return "\(Int(secs))s" }
        let mins = Int(secs / 60)
        if mins < 60 { return "~\(mins)m" }
        let hrs = mins / 60
        let rem = mins % 60
        return rem == 0 ? "~\(hrs)h" : "~\(hrs)h \(rem)m"
    }

    // MARK: Current-setup descriptions

    private var sttDescription: String {
        let mode = appState.sttMode == "local" ? "Local" : "Cloud"
        let provider = appState.sttProvider.displayName
        let model = appState.apiModel
        return appState.sttMode == "local"
            ? "On device · \(appState.localModel)"
            : "\(provider) · \(model) · \(mode)"
    }

    private var outputDescription: String {
        guard let entry = MacLlmStyles.first(where: { $0.key == appState.llmStyle }) else {
            return appState.llmStyle.capitalized
        }
        if appState.llmEnabled == false {
            return "Off"
        }
        let providerLabel = appState.llmMode == "local"
            ? "On device · \(appState.localLlmModel)"
            : "Cloud · \(appState.llmApiModel.split(separator: "-").first.map(String.init) ?? appState.llmApiModel)"
        return "\(entry.label) · \(providerLabel)"
    }
}

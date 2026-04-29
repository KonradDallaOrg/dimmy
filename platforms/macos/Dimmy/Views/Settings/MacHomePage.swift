import SwiftUI

// Home — landing tab. Three blocks:
//   1. Hero (welcome + shortcut hint + Test microphone / Change shortcut buttons)
//   2. Stats grid (words / speaking time / time saved)
//   3. "Current setup" tile (STT provider, output style, shortcut summary)
//
// The "Recent dictations" tile from the design is omitted for now — the
// macOS app already has a full History tab, and the Home recent list
// would duplicate that data. We can add a 3-row preview here in Phase 6
// polish if the user wants it.

struct MacHomePage: View {
    @ObservedObject var appState: AppState
    let onTabChange: (MacSettingsTab) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            hero
                .padding(.bottom, 8)

            statsGrid
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
            }
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
                    .fill(Color.black.opacity(0.06))
                    .frame(width: 200, height: 96)
                if let nsImage = NSImage(named: NSImage.applicationIconName) {
                    Image(nsImage: nsImage)
                        .resizable()
                        .aspectRatio(contentMode: .fit)
                        .frame(width: 80, height: 80)
                        .shadow(color: Color.accentColor.opacity(0.35),
                                radius: 12, x: 0, y: 6)
                } else {
                    Image(systemName: "waveform.circle.fill")
                        .font(.system(size: 64))
                        .foregroundStyle(Color.accentColor)
                }
            }
        }
    }

    private var heroActions: some View {
        HStack(spacing: 8) {
            Button {
                // Phase 2: wire to dimmy_check_audio_health.
                // For now, navigate to Voice input where Input level meter lives.
                onTabChange(.voice)
            } label: {
                Label("Test microphone", systemImage: "mic.fill")
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.regular)

            Button("Change shortcut…") { onTabChange(.shortcut) }
                .controlSize(.regular)
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
        if secs < 1 { return "—" }
        let mins = Int(secs / 60)
        let remSecs = Int(secs.truncatingRemainder(dividingBy: 60))
        if mins == 0 { return "\(remSecs)s" }
        return String(format: "%d:%02d", mins, remSecs)
    }

    private var timeSavedText: String {
        // Same heuristic as Windows: typing ~40 WPM vs dictation ~150 WPM.
        let words = Double(appState.statsTotalWords)
        let secs = words * (1.0 / 40 - 1.0 / 150) * 60
        if secs < 1 { return "—" }
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

import SwiftUI

// About, hero with app icon + version + check-for-updates / release
// notes, then Updates settings (auto-update, channel) and Resources
// links. Footer credit "Made with [Anthropic mark] Claude Code" matches
// the Windows v3 footer.

struct MacAboutPage: View {
    @ObservedObject var appState: AppState
    @ObservedObject private var updates = UpdateService.shared
    @State private var legalSheet: LegalSheetKind?

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            heroCard
                .padding(.bottom, 8)

            // Updates group has Status (Simple) + Update channel.
            // Update channel is Advanced per Win XAML (visibility
            // bound to IsAdvanced) since it controls whether the
            // user sees prerelease builds. Status alone stays
            // visible in Simple.
            statusOnlyGroup
            // Channel picker is meaningless without the auto_update
            // scope — Windows collapses `UpdateChannelCard` for the
            // same reason.
            if appState.showAdvanced && updates.isLicensed {
                updateChannelGroup
            }

            setupGroup
            resourcesGroup
            legalGroup
            // "Made with Claude Code" credit removed per Win redesign
            // (commit 4080ff06).
        }
        .sheet(item: $legalSheet) { kind in
            LegalSheet(title: kind.title, text: kind.text) { legalSheet = nil }
        }
    }

    // MARK: Setup

    // Mirror of the Windows About setup cards. Reopening any of these is
    // idempotent: they re-detect an already-saved key, already-downloaded
    // models and an already-bound shortcut, so nothing is re-downloaded and
    // no key is asked for twice.
    //
    // The first card flips `isOnboardingComplete` to false so the finish step
    // can flip it back to true and trigger the window close, the same
    // handshake the Diagnostics "Reset onboarding" uses. The two wizards need
    // no such dance: they are standalone windows that close themselves.
    private var setupGroup: some View {
        Group {
            MacGroupLabel(text: "Setup")
            HStack(alignment: .top, spacing: 12) {
                MacWizardCard(
                    icon: "sparkles",
                    iconBackground: Color(red: 0.04, green: 0.52, blue: 1.00),
                    title: "Run setup",
                    description: "Model, key, shortcut."
                ) {
                    appState.resetOnboarding()
                    AppDelegate.shared?.reopenOnboarding()
                }
                MacWizardCard(
                    icon: "text.cursor",
                    iconBackground: Color(red: 0.55, green: 0.35, blue: 0.95),
                    title: "Command mode",
                    description: "Edit selected text by voice."
                ) {
                    WizardWindowController.shared.show(.command, appState: appState)
                }
                MacWizardCard(
                    icon: "waveform",
                    iconBackground: Color(red: 0.94, green: 0.36, blue: 0.24),
                    title: "Meeting mode",
                    description: "Record, then get a recap."
                ) {
                    WizardWindowController.shared.show(.meeting, appState: appState)
                }
            }
            // Same height for all three whatever the text length: each card
            // fills the row, and the row sizes to its tallest card.
            .fixedSize(horizontal: false, vertical: true)
        }
    }

    private var statusOnlyGroup: some View {
        Group {
            MacGroupLabel(text: "Updates")
            MacTile {
                MacRow(
                    "Status",
                    description: updates.statusText,
                    showsDivider: false
                ) {
                    if updates.isChecking {
                        ProgressView()
                            .controlSize(.small)
                    } else if updates.canInstallNow {
                        Button("Install and relaunch") { updates.installNow() }
                            .buttonStyle(.borderedProminent)
                    } else if updates.isUpdateReady {
                        Circle()
                            .fill(Color.accentColor)
                            .frame(width: 8, height: 8)
                    } else {
                        EmptyView()
                    }
                }
            }
        }
    }

    private var updateChannelGroup: some View {
        Group {
            MacGroupLabel(text: "Channel")
            MacTile {
                MacRow(
                    "Update channel",
                    hint: "Stable only is the safer default. Prerelease also offers staging builds with new features earlier.",
                    hintURL: URL(string: "https://dimmy.app/help/about-and-updates"),
                    showsDivider: false
                ) {
                    Picker("", selection: $updates.channel) {
                        Text("Stable").tag("stable")
                        Text("Prerelease").tag("prerelease")
                    }
                    .labelsHidden()
                    .frame(width: 140)
                }
            }
        }
    }

    // MARK: Hero

    private var heroCard: some View {
        MacHero(
            title: "Dimmy \(versionString)",
            subtitle: "Voice dictation that stays out of your way.\nReleased \(buildDateString).",
            actions: AnyView(
                HStack(spacing: 8) {
                    Button {
                        updates.checkForUpdatesNow()
                    } label: {
                        Label(updates.isChecking ? "Checking..." : "Check for updates...",
                              systemImage: "arrow.down.circle.fill")
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(updates.isChecking)

                    Button {
                        if let url = URL(string: "https://github.com/KonradDallaOrg/dimmy/releases") {
                            NSWorkspace.shared.open(url)
                        }
                    } label: {
                        Label("View release notes", systemImage: "doc.text")
                    }
                }
            )
        ) {
            ZStack {
                // Theme-aware brand squircle: white squircle + gradient
                // glyph in light, gradient squircle + white glyph in dark
                // (DimmyAppIcon imageset has both appearances). The Dock's
                // AppIcon is the light variant; this in-app copy follows
                // the system appearance so it reads on either surface.
                Image("DimmyAppIcon")
                    .resizable()
                    .aspectRatio(contentMode: .fit)
                    .frame(width: 96, height: 96)
                    .shadow(color: Color.accentColor.opacity(0.30),
                            radius: 12, x: 0, y: 6)
            }
            .frame(width: 200, height: 110)
        }
    }

    // MARK: Updates

    // Legacy combined group replaced by statusOnlyGroup +
    // updateChannelGroup (channel gated behind Advanced).

    // MARK: Resources

    private var resourcesGroup: some View {
        Group {
            MacGroupLabel(text: "Resources")
            MacTile {
                MacRow(
                    "Documentation",
                    description: "Guides, shortcuts, troubleshooting",
                    icon: "globe",
                    iconBackground: Color(red: 0.04, green: 0.52, blue: 1.00)
                ) {
                    Link(destination: URL(string: "https://dimmy.app/docs")!) {
                        Text("dimmy.app/docs ›")
                            .font(.system(size: 12))
                    }
                }
                MacRow(
                    "Source code of this version",
                    description: "Source, issues, releases",
                    icon: "chevron.left.forwardslash.chevron.right",
                    iconBackground: Color(red: 0.11, green: 0.11, blue: 0.12),
                    showsDivider: false
                ) {
                    Link(destination: URL(string: "https://github.com/KonradDallaOrg/dimmy/tree/v\(versionString)")!) {
                        Text("github.com ›")
                            .font(.system(size: 12))
                    }
                }
            }
        }
    }

    // MARK: Legal

    // AGPL-3.0 sections 0 and 5(d): the notice, the no-warranty statement and
    // the licence texts, reachable from the running app.
    private var legalGroup: some View {
        Group {
            MacGroupLabel(text: "Legal")
            Text("Dimmy v\(versionString) \u{00A9} Konrad Dalla. Free software under the GNU AGPL v3. NO WARRANTY.")
                .font(.system(size: 12))
                .foregroundStyle(Color.macTextSecondary)
                .padding(.bottom, 8)
            MacTile {
                MacRow(
                    "Licence (AGPL-3.0)",
                    description: "The full licence text",
                    icon: "doc.text.fill",
                    iconBackground: Color.gray
                ) {
                    Button("View") { legalSheet = .license }
                }
                MacRow(
                    "Open source notices",
                    description: "Licences of the components Dimmy includes",
                    icon: "doc.on.doc.fill",
                    iconBackground: Color.gray,
                    showsDivider: false
                ) {
                    Button("View") { legalSheet = .notices }
                }
            }
        }
    }

    // MARK: Footer credit

    private var anthropicCredit: some View {
        HStack(spacing: 6) {
            Text("Made with")
                .font(.system(size: 12))
                .foregroundStyle(Color.macTextTertiary)
            // Same SVG path data as Windows About panel, bundled as a
            // vector asset so it stays crisp at any size.
            Image("ClaudeMark")
                .resizable()
                .aspectRatio(contentMode: .fit)
                .frame(width: 14, height: 14)
            Text("Claude Code")
                .font(.system(size: 12, weight: .semibold))
                .foregroundStyle(Color.macTextSecondary)
        }
        .frame(maxWidth: .infinity)
        .padding(.top, 24)
        .padding(.bottom, 4)
    }

    // MARK: Helpers

    /// Shows the BUILD version (`0.6.74-rc.2`), not just the marketing
    /// number — rc.1 and rc.2 print the same `CFBundleShortVersionString`,
    /// so a tester could not tell which one they had installed.
    private var versionString: String {
        UpdateService.runningVersionPlain
    }

    private var buildDateString: String {
        if let infoPath = Bundle.main.path(forResource: "Info", ofType: "plist"),
           let attrs = try? FileManager.default.attributesOfItem(atPath: infoPath),
           let date = attrs[.creationDate] as? Date {
            let formatter = DateFormatter()
            formatter.dateFormat = "MMMM d, yyyy"
            return formatter.string(from: date)
        }
        return "this build"
    }
}

// MARK: - Legal text sheet

private enum LegalSheetKind: String, Identifiable {
    case license
    case notices

    var id: String { rawValue }

    var title: String {
        switch self {
        case .license: return "Licence (AGPL-3.0)"
        case .notices: return "Open source notices"
        }
    }

    var text: String {
        let raw = self == .license
            ? DimmyCore.shared.licenseText()
            : DimmyCore.shared.thirdPartyNotices()
        return raw ?? "Text unavailable."
    }
}

private struct LegalSheet: View {
    let title: String
    let text: String
    let onClose: () -> Void

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Text(title).font(.headline)
                Spacer()
                Button("Close", action: onClose)
                    .keyboardShortcut(.cancelAction)
            }
            .padding(12)
            Divider()
            LegalTextView(text: text)
        }
        .frame(minWidth: 640, minHeight: 480)
    }
}

/// NSTextView, not SwiftUI Text: the notices are ~1 MB.
private struct LegalTextView: NSViewRepresentable {
    let text: String

    func makeNSView(context: Context) -> NSScrollView {
        let scroll = NSTextView.scrollableTextView()
        if let tv = scroll.documentView as? NSTextView {
            tv.isEditable = false
            tv.isSelectable = true
            tv.font = .monospacedSystemFont(ofSize: 11, weight: .regular)
            tv.textContainerInset = NSSize(width: 8, height: 8)
            tv.string = text
        }
        return scroll
    }

    func updateNSView(_ nsView: NSScrollView, context: Context) {}
}

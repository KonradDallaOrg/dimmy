import SwiftUI

// About, hero with app icon + version + check-for-updates / release
// notes, then Updates settings (auto-update, channel) and Resources
// links. Footer credit "Made with [Anthropic mark] Claude Code" matches
// the Windows v3 footer.

struct MacAboutPage: View {
    @ObservedObject var appState: AppState
    @ObservedObject private var updates = UpdateService.shared

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
            // "Made with Claude Code" credit removed per Win redesign
            // (commit 4080ff06).
        }
    }

    // MARK: Setup

    // Mirror of the Windows About "Run setup again" card. Reopening the
    // wizard is idempotent: its onAppear re-detects an already-saved key
    // and already-downloaded models, so nothing is re-downloaded and no
    // key is asked for twice. We flip `isOnboardingComplete` to false so
    // the finish step can flip it back to true and trigger the window
    // close, same handshake the Diagnostics "Reset onboarding" uses.
    private var setupGroup: some View {
        Group {
            MacGroupLabel(text: "Setup")
            MacTile {
                MacRow(
                    "Run setup again",
                    description: "Pick your model, key, and shortcut. Saved keys and downloaded models are kept.",
                    icon: "sparkles",
                    iconBackground: Color(red: 0.04, green: 0.52, blue: 1.00),
                    showsDivider: false
                ) {
                    Button("Open setup") {
                        appState.isOnboardingComplete = false
                        AppDelegate.shared?.reopenOnboarding()
                    }
                }
            }
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
                    "GitHub repository",
                    description: "Source, issues, releases",
                    icon: "chevron.left.forwardslash.chevron.right",
                    iconBackground: Color(red: 0.11, green: 0.11, blue: 0.12)
                ) {
                    Link(destination: URL(string: "https://github.com/KonradDallaOrg/dimmy")!) {
                        Text("github.com ›")
                            .font(.system(size: 12))
                    }
                }
                MacRow(
                    "License",
                    description: "Open source, free to fork",
                    icon: "doc.text.fill",
                    iconBackground: Color.gray,
                    showsDivider: false
                ) {
                    Link(destination: URL(string: "https://github.com/KonradDallaOrg/dimmy/blob/main/LICENSE")!) {
                        Text("View ›").font(.system(size: 12))
                    }
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

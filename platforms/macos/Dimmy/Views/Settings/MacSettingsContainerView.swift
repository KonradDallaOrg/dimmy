import SwiftUI

// Tahoe-style Settings container.
//
// Layout matches the design handoff `mac-app.jsx`:
// - Liquid-glass sidebar (240 px) with traffic-lights area + search + nav
// - Main pane with toolbar (back/forward chevrons + title + saved-pulse +
//   inline status pill) and a scrollable content area
// - Sidebar footer with version + Advanced toggle
//
// We do NOT render fake traffic lights, the SwiftUI Settings scene gives
// us the real ones for free at the window chrome level. The sidebar
// reserves space at the top so visual alignment matches.

enum MacSettingsTab: String, CaseIterable, Identifiable {
    // Sidebar order mirrors Windows (SettingsWindow.xaml NavigationView.MenuItems):
    // Home → Voice input → Output → Pill overlay → App rules → Shortcut →
    // Recordings → Integrations → Privacy & data → License → About → Advanced.
    // Permissions is Mac-only (no Win analogue, TCC vs UAC model differ);
    // we tuck it next to Privacy where it semantically belongs.
    case home, voice, output, providers, pill, rules, shortcut, history,
         integrations, privacy, permissions, license, about, advanced

    var id: String { rawValue }

    var label: String {
        switch self {
        case .home:         return "Home"
        case .voice:        return "Voice input"
        case .output:       return "Output"
        case .providers:    return "Providers & keys"
        case .rules:        return "App rules"
        case .history:      return "Recordings"
        case .integrations: return "Integrations"
        case .pill:         return "Pill overlay"
        case .shortcut:     return "Shortcut"
        case .permissions:  return "Permissions"
        case .privacy:      return "Privacy & data"
        case .license:      return "License"
        case .about:        return "About"
        case .advanced:     return "Debug"
        }
    }

    var subtitle: String {
        switch self {
        case .home:         return "Get a quick read on Dimmy and jump to what you need."
        case .voice:        return "How Dimmy hears and transcribes your voice."
        case .output:       return "How Dimmy rewrites and delivers your dictation."
        case .providers:    return "One card per AI provider, one key per provider."
        case .rules:        return "Auto-switch the rewrite style based on the focused app."
        case .history:      return "Past dictations, click a row to see Raw / Enhanced and replay audio."
        case .integrations: return "Push your meeting recaps into the tools you already use."
        case .pill:         return "The floating pill, where it lives and how it looks."
        case .shortcut:     return "The hotkey that starts and stops recording."
        case .permissions:  return "macOS access Dimmy needs to record and paste."
        case .privacy:      return "What leaves your machine, and what doesn't."
        case .license:      return "Activate Dimmy, manage your trial, or paste an activation code from email."
        case .about:        return "Version, updates, and resources."
        case .advanced:     return "Developer-leaning controls and diagnostics."
        }
    }

    /// SF Symbol for the squircle nav icon.
    var iconSystemName: String {
        switch self {
        case .home:         return "house.fill"
        case .voice:        return "mic.fill"
        case .output:       return "text.bubble.fill"
        case .providers:    return "key.horizontal.fill"
        case .rules:        return "rectangle.3.group.fill"
        case .history:      return "clock.arrow.circlepath"
        case .integrations: return "link"
        case .pill:         return "capsule.fill"
        case .shortcut:     return "keyboard.fill"
        case .permissions:  return "hand.raised.fill"
        case .privacy:      return "lock.shield.fill"
        case .license:      return "key.fill"
        case .about:        return "info.circle.fill"
        case .advanced:     return "wrench.and.screwdriver.fill"
        }
    }

    var iconColor: Color {
        switch self {
        case .home:         return Color(red: 0.04, green: 0.52, blue: 1.00)
        case .voice:        return Color(red: 1.00, green: 0.22, blue: 0.37)
        case .output:       return Color(red: 1.00, green: 0.80, blue: 0.00)
        case .providers:    return Color(red: 0.48, green: 0.55, blue: 1.00)
        case .rules:        return Color(red: 0.20, green: 0.78, blue: 0.35)
        case .history:      return Color(red: 0.34, green: 0.61, blue: 0.99)
        case .integrations: return Color(red: 0.20, green: 0.20, blue: 0.20)  // Notion-mark black
        case .pill:         return Color(red: 0.69, green: 0.32, blue: 0.87)
        case .shortcut:     return Color(red: 0.04, green: 0.52, blue: 1.00)
        case .permissions:  return Color(red: 1.00, green: 0.45, blue: 0.20)
        case .privacy:      return Color(red: 0.11, green: 0.11, blue: 0.12)
        case .license:      return Color(red: 1.00, green: 0.62, blue: 0.04)
        case .about:        return Color(red: 0.56, green: 0.56, blue: 0.58)
        case .advanced:     return Color(red: 1.00, green: 0.62, blue: 0.04)
        }
    }
}

// MARK: - Container
//
// Holds the navigation history (so back/forward in the toolbar work),
// search filter, saved-pulse animation, and the per-tab dispatch.
//
// `appState` carries the shared ViewModel; pages bind directly to its
// `@Published` fields and call FFI through `DimmyCore` on change. The
// container itself only owns transient UI state.

struct MacSettingsContainerView: View {
    @ObservedObject var appState: AppState

    @State private var current: MacSettingsTab = .home
    @State private var search: String = ""
    @State private var savedPulse: Bool = false
    @State private var savedPulseTask: Task<Void, Never>?

    /// Chevrons walk the visible sidebar order (filtered by Advanced + search).
    /// Disabled at the edges, no wraparound, matches macOS Settings.app.
    private var canGoBack: Bool {
        guard let i = filteredTabs.firstIndex(of: current) else { return false }
        return i > 0
    }
    private var canGoForward: Bool {
        guard let i = filteredTabs.firstIndex(of: current) else { return false }
        return i < filteredTabs.count - 1
    }

    /// Visible tabs filtered by search + Advanced toggle.
    ///
    /// Per docs/dev/settings-map.md (the user-filled Simple/Advanced
    /// matrix) and Win SettingsWindow.xaml nav definition:
    /// - Always visible (Simple): home, voice, output, shortcut,
    ///   license, privacy, about, permissions
    /// - Behind Advanced toggle: pill, rules, history (Recordings),
    ///   integrations, advanced (Debug)
    /// - .providers will land here once MacProvidersPage exists.
    private static let advancedOnlyTabs: Set<MacSettingsTab> = [
        .pill, .rules, .history, .integrations, .advanced
    ]

    private var filteredTabs: [MacSettingsTab] {
        let q = search.trimmingCharacters(in: .whitespaces).lowercased()
        return MacSettingsTab.allCases.filter { tab in
            if Self.advancedOnlyTabs.contains(tab) && !appState.showAdvanced {
                return false
            }
            if q.isEmpty { return true }
            return tab.label.lowercased().contains(q)
        }
    }

    var body: some View {
        HStack(spacing: 0) {
            sidebar
            mainPane
        }
        .frame(minWidth: 1000, minHeight: 740)
        .background(.ultraThinMaterial)
        .onChange(of: appState.showAdvanced) { _, newValue in
            // If user just toggled Advanced off while Advanced tab is the
            // active one, fall back to Home so we never render a hidden tab.
            if !newValue && current == .advanced { goTo(.home) }
        }
        .onReceive(NotificationCenter.default.publisher(for: .dimmyOpenLicenseTab)) { _ in
            // AppDelegate posts this after dimmy:// activation succeeds ,
            // bring the user to the License page so they see the result.
            goTo(.license)
        }
        .onReceive(NotificationCenter.default.publisher(for: .dimmyNavigateSettingsTab)) { note in
            // Drives SettingsScreenshotter: navigate to an arbitrary
            // tab by raw value posted in userInfo["tab"]. No-op for
            // unknown raw values, so a stale shooter cannot land us
            // on a missing tab.
            if let raw = note.userInfo?["tab"] as? String,
               let tab = MacSettingsTab(rawValue: raw) {
                current = tab
            }
        }
    }

    // MARK: Sidebar

    private var sidebar: some View {
        VStack(alignment: .leading, spacing: 0) {
            // Reserve space for traffic lights (real ones rendered by the
            // window chrome above this view). 56 px matches design.
            Spacer().frame(height: 56)

            // Search field
            HStack(spacing: 6) {
                Image(systemName: "magnifyingglass")
                    .font(.system(size: 11))
                    .foregroundStyle(Color.macTextSecondary)
                TextField("Search", text: $search)
                    .textFieldStyle(.plain)
                    .font(.system(size: 12))
            }
            .padding(.horizontal, 12)
            .frame(height: 30)
            .background(Color.white.opacity(0.5).blendMode(.plusLighter))
            .overlay(
                Capsule().stroke(Color.white.opacity(0.5), lineWidth: 0.5)
            )
            .clipShape(Capsule())
            .padding(.horizontal, 14)
            .padding(.bottom, 10)

            // Nav items
            ScrollView {
                VStack(alignment: .leading, spacing: 2) {
                    ForEach(filteredTabs) { tab in
                        navItem(for: tab)
                    }
                }
                .padding(.horizontal, 10)
                .padding(.bottom, 10)
            }

            Divider().opacity(0.5)

            // Staging watermark, visible only on builds with
            // DIMMY_BUILD_FLAVOR=staging. Mirror of the Win sidebar
            // banner (SettingsWindow.xaml::StagingBanner). The yellow
            // stripe + warning glyph is intentional: a tester running
            // staging side-by-side with a prod install must spot the
            // difference at a glance.
            if DimmyCore.shared.isStagingBuild {
                VStack(alignment: .leading, spacing: 2) {
                    HStack(spacing: 6) {
                        Image(systemName: "exclamationmark.triangle.fill")
                            .font(.system(size: 11, weight: .semibold))
                            .foregroundStyle(Color(red: 0.66, green: 0.43, blue: 0.0))
                        Text("STAGING BUILD")
                            .font(.system(size: 11, weight: .semibold))
                            .foregroundStyle(Color(red: 0.66, green: 0.43, blue: 0.0))
                    }
                    Text("Test environment · Stripe TEST · payments simulated")
                        .font(.system(size: 10))
                        .foregroundStyle(Color.macTextSecondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
                .padding(EdgeInsets(top: 10, leading: 14, bottom: 10, trailing: 14))
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(Color.yellow.opacity(0.18))
                .overlay(
                    Rectangle()
                        .fill(Color(red: 0.66, green: 0.43, blue: 0.0).opacity(0.4))
                        .frame(height: 0.5),
                    alignment: .top
                )
                .overlay(
                    Rectangle()
                        .fill(Color(red: 0.66, green: 0.43, blue: 0.0).opacity(0.4))
                        .frame(height: 0.5),
                    alignment: .bottom
                )
            }

            // Footer: version + Advanced toggle
            HStack {
                HStack(spacing: 6) {
                    Circle()
                        .fill(Color.green)
                        .frame(width: 10, height: 10)
                        .shadow(color: .green.opacity(0.6), radius: 4)
                    Text(currentVersionString)
                        .font(.system(size: 12))
                        .foregroundStyle(Color.macTextSecondary)
                }
                Spacer()
                Toggle("Advanced", isOn: $appState.showAdvanced)
                    .toggleStyle(.switch)
                    .controlSize(.small)
                    .font(.system(size: 11))
            }
            .padding(EdgeInsets(top: 10, leading: 14, bottom: 14, trailing: 14))
        }
        .frame(width: MacTheme.sidebarWidth)
        .background(.thinMaterial)
        .overlay(alignment: .trailing) {
            // Right-edge hairline divider matches design's `--sidebar-shadow`.
            Rectangle()
                .fill(Color.primary.opacity(0.08))
                .frame(width: 0.5)
        }
    }

    @ViewBuilder
    private func navItem(for tab: MacSettingsTab) -> some View {
        let isActive = tab == current
        Button { goTo(tab) } label: {
            HStack(spacing: 10) {
                MacSquircleIcon(systemName: tab.iconSystemName,
                                background: tab.iconColor)
                Text(tab.label)
                    .font(.system(size: 13))
                    .foregroundStyle(.primary)
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 10)
            .frame(maxWidth: .infinity, alignment: .leading)
            .frame(height: MacTheme.navItemHeight)
            .background(
                RoundedRectangle(cornerRadius: 10, style: .continuous)
                    .fill(isActive ? Color.white.opacity(0.45)
                                   : Color.clear)
            )
            .overlay(
                // Soft inset highlight when active, matches Tahoe nav-active.
                RoundedRectangle(cornerRadius: 10, style: .continuous)
                    .strokeBorder(Color.white.opacity(isActive ? 0.6 : 0),
                                  lineWidth: 0.5)
            )
            .contentShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
        }
        .buttonStyle(.plain)
    }

    // MARK: Main pane

    private var mainPane: some View {
        VStack(spacing: 0) {
            toolbar
            ScrollView {
                VStack(alignment: .leading, spacing: 0) {
                    pageHead
                    page
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, MacTheme.pageHorizontalPadding)
                .padding(.top, MacTheme.pageTopPadding)
                .padding(.bottom, MacTheme.pageBottomPadding)
            }
        }
        .background(.regularMaterial)
    }

    private var toolbar: some View {
        HStack {
            HStack(spacing: 10) {
                chevronGroup
                Text(current.label)
                    .font(.system(size: 13, weight: .semibold))
            }
            Spacer()
            HStack(spacing: 8) {
                if savedPulse {
                    MacSavedPulse().transition(.opacity)
                }
                MacStatusPill(text: providerStatusText, dotColor: .green)
            }
        }
        .padding(.horizontal, MacTheme.pageHorizontalPadding)
        .frame(height: 56)
        .overlay(alignment: .bottom) {
            Rectangle()
                .fill(Color.macRowDivider)
                .frame(height: 0.5)
                .padding(.horizontal, 18)
                .opacity(0.5)
        }
    }

    private var chevronGroup: some View {
        HStack(spacing: 0) {
            Button { goBack() } label: {
                Image(systemName: "chevron.left")
                    .font(.system(size: 11, weight: .semibold))
                    .frame(width: 30, height: 24)
            }
            .buttonStyle(.plain)
            .disabled(!canGoBack)
            .opacity(canGoBack ? 1.0 : 0.4)

            Button { goForward() } label: {
                Image(systemName: "chevron.right")
                    .font(.system(size: 11, weight: .semibold))
                    .frame(width: 30, height: 24)
            }
            .buttonStyle(.plain)
            .disabled(!canGoForward)
            .opacity(canGoForward ? 1.0 : 0.4)
        }
        .padding(2)
        .background(
            Capsule().fill(Color.gray.opacity(0.18))
        )
    }

    private var pageHead: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(current.label)
                .font(.system(size: 22, weight: .bold))
                .tracking(-0.22)
            Text(current.subtitle)
                .font(.system(size: 13))
                .foregroundStyle(Color.macTextSecondary)
        }
        .padding(.bottom, MacTheme.pageHeadBottomPadding)
        .padding(.top, 8)
    }

    @ViewBuilder
    private var page: some View {
        switch current {
        case .home:        MacHomePage(appState: appState, onTabChange: goTo)
        case .voice:       MacVoicePage(appState: appState)
        case .output:      MacOutputPage(appState: appState)
        case .providers:   MacProvidersPage(appState: appState)
        case .rules:       MacRulesPage(appState: appState)
        case .history:     HistorySettingsView(appState: appState)
                              .frame(minHeight: 480)
        case .integrations: MacIntegrationsPage(appState: appState)
        case .pill:        MacPillPage(appState: appState)
        case .shortcut:    MacShortcutPage(appState: appState)
        case .permissions: MacPermissionsPage(appState: appState)
        case .privacy:     MacPrivacyPage(appState: appState)
        case .license:     MacLicensePage(appState: appState)
        case .about:       MacAboutPage(appState: appState)
        case .advanced:    MacAdvancedPage(appState: appState)
        }
    }

    // MARK: Navigation history

    private func goTo(_ tab: MacSettingsTab) {
        current = tab
    }

    private func goBack() {
        guard let i = filteredTabs.firstIndex(of: current), i > 0 else { return }
        current = filteredTabs[i - 1]
    }

    private func goForward() {
        guard let i = filteredTabs.firstIndex(of: current),
              i < filteredTabs.count - 1 else { return }
        current = filteredTabs[i + 1]
    }

    // MARK: Saved-pulse trigger

    /// Public entry point pages call after writing to AppState. Cancels
    /// any in-flight pulse so rapid changes don't double-animate.
    func triggerSavedPulse() {
        savedPulseTask?.cancel()
        savedPulse = true
        savedPulseTask = Task { @MainActor in
            try? await Task.sleep(nanoseconds: 1_500_000_000)
            if !Task.isCancelled { savedPulse = false }
        }
    }

    // MARK: Helpers

    /// Build version, so an rc is identifiable at a glance — see
    /// `UpdateService.displayVersion`.
    private var currentVersionString: String {
        UpdateService.runningVersion
    }

    private var providerStatusText: String {
        // Mirrors design: "● Listening, groq whisper". Falls back to
        // model name when provider isn't recognised (custom endpoint).
        let provider = appState.sttProvider.displayName
        let model = appState.apiModel
        return "\(provider), \(model.split(separator: "-").first.map(String.init) ?? model)"
            .lowercased()
    }
}

// Placeholder used until each page is implemented in Phase 2+.
struct MacPlaceholderPage: View {
    let title: String
    let message: String

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            MacNote(
                title: title,
                message: message,
                systemImage: "hammer.fill"
            )
            Spacer(minLength: 240)
        }
    }
}

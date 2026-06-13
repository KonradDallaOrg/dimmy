import SwiftUI

/// Settings → Integrations page. Mac mirror of the Win
/// `IntegrationsPanel`, summary card + 3-step sheet wizard
/// (`NotionConnectSheet`) for full setup. Re-runnable: "Change
/// destination" reopens the wizard at step 3.
struct MacIntegrationsPage: View {
    @ObservedObject var appState: AppState

    @State private var showWizard: Bool = false
    @State private var wizardInitialStep: Int = 1
    @State private var statusMessage: String = ""
    @State private var statusIsError: Bool = false
    @State private var showDisconnectConfirm: Bool = false
    @State private var showClaudeWizard: Bool = false
    @State private var showMcpWizard: Bool = false
    @State private var mcpStatus: DimmyCore.ClaudeDesktopStatus = .empty
    @State private var mcpRefreshTimer: Timer? = nil
    @State private var showMcpDisconnectConfirm: Bool = false
    @State private var claudeIconPath: String? = nil

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            MacGroupLabel(text: "Anthropic, Claude Code subscription")
            // Detailed status + sign-in card. Sign-in opens the
            // browser via `claude /login` running in Terminal, the
            // token is stored either in `~/.claude/credentials.json`
            // or (recent CLIs) macOS Keychain under service
            // "Claude Code-credentials". Dimmy never touches the
            // token directly; only probes existence.
            MacClaudeCodeCard(
                appState: appState,
                onWizardRequested: { showClaudeWizard = true }
            )
            MacGroupFooter(text: "OAuth token stays with the `claude` CLI in macOS Keychain (or ~/.claude/credentials.json). Dimmy only checks existence, never reads the token.")

            Spacer().frame(height: 24)
            MacGroupLabel(text: "OpenAI subscription")
            // Use the user's ChatGPT plan via the local `codex` CLI —
            // mirror of the Claude Code card above. Dimmy never reads
            // ~/.codex/auth.json; the CLI owns the token.
            MacCodexCard(appState: appState)
            MacGroupFooter(text: "Uses your ChatGPT plan through the `codex` CLI, no API key spent. The OAuth token stays with Codex in ~/.codex. Dimmy only checks that you're signed in.")

            Spacer().frame(height: 24)
            MacGroupLabel(text: "Notion")

            // Summary card, current state + action buttons.
            summaryCard

            // Auto-send toggle, only meaningful once connected.
            // Stays inline (not in wizard) because users may flip it
            // on/off over time without re-entering setup.
            Spacer().frame(height: 16)
            MacGroupLabel(text: "Automation")
            MacTile {
                MacRow(
                    "Auto-send each meeting",
                    hint: "When on, every meeting's recap uploads to your Notion destination as soon as the recap finishes. When off, you click Send to Notion from the meeting Done view per meeting.",
                    showsDivider: false
                ) {
                    Toggle("", isOn: Binding(
                        get: { appState.notionAutoSend },
                        set: { newValue in
                            appState.notionAutoSend = newValue
                            DimmyCore.shared.setConfig(appState.toRustConfig())
                        }
                    ))
                    .labelsHidden()
                    .toggleStyle(.switch)
                    .disabled(!appState.hasNotionToken)
                }
            }

            // Status message bar
            if !statusMessage.isEmpty {
                Spacer().frame(height: 16)
                MacNote(
                    title: statusIsError ? "There was a problem" : "Notion",
                    message: statusMessage,
                    systemImage: statusIsError ? "exclamationmark.triangle.fill" : "checkmark.circle.fill"
                )
            }

            // Help footer
            Spacer().frame(height: 16)
            HStack(spacing: 4) {
                Text("Free Notion accounts work. Token + destination stay on this Mac.")
                    .font(.system(size: 11))
                    .foregroundStyle(.secondary)
                MacInfoButton(text: "Free Notion plans have no per-plan API limits, only the standard 3 requests/sec rate limit. Token + destination are stored locally; only the recap markdown leaves this Mac when you (or auto-send) trigger an upload.")
            }
            .fixedSize(horizontal: false, vertical: true)

            Spacer().frame(height: 24)
            MacGroupLabel(text: "Claude Desktop (MCP)")
            mcpCard
            MacGroupFooter(text: "Claude Desktop spawns Dimmy's MCP binary locally, no network round-trip. Disconnect removes the entry from claude_desktop_config.json.")
        }
        .sheet(isPresented: $showWizard) {
            NotionConnectSheet(
                appState: appState,
                initialStep: wizardInitialStep,
                onClose: {
                    showWizard = false
                    if appState.hasNotionToken && !appState.notionTargetTitle.isEmpty {
                        statusIsError = false
                        statusMessage = "Recaps will land in '\(appState.notionTargetTitle)'."
                    }
                }
            )
        }
        .sheet(isPresented: $showClaudeWizard) {
            ClaudeConnectSheet(
                appState: appState,
                onClose: {
                    showClaudeWizard = false
                    DimmyCore.shared.recheckClaudeCode()
                    appState.refreshClaudeCodeStatus()
                },
                onComplete: { ok in
                    if ok {
                        // Flip llm_auth_method=subscription so the user
                        // sees the integration go live without a
                        // second click in Output → LLM. Mirror of the
                        // Win wizard completion path.
                        appState.llmAuthMethod = "subscription"
                        DimmyCore.shared.setConfig(appState.toRustConfig())
                        DimmyCore.shared.trackEvent("claude_code.wizard_completed")
                    }
                }
            )
        }
        .confirmationDialog(
            "Disconnect Notion?",
            isPresented: $showDisconnectConfirm,
            titleVisibility: .visible
        ) {
            Button("Disconnect", role: .destructive) { disconnect() }
            Button("Cancel", role: .cancel) { }
        } message: {
            Text("Dimmy will forget your token and destination. Your Notion content stays untouched.")
        }
        .sheet(isPresented: $showMcpWizard) {
            ClaudeDesktopConnectSheet(
                appState: appState,
                onClose: {
                    showMcpWizard = false
                    refreshMcpStatus()
                },
                onComplete: { _ in refreshMcpStatus() }
            )
        }
        .confirmationDialog(
            "Disconnect Claude Desktop?",
            isPresented: $showMcpDisconnectConfirm,
            titleVisibility: .visible
        ) {
            Button("Disconnect", role: .destructive) {
                _ = DimmyCore.shared.uninstallClaudeDesktopExtension()
                refreshMcpStatus()
            }
            Button("Cancel", role: .cancel) { }
        } message: {
            Text("Dimmy will remove its entry from Claude Desktop's MCP config. Your other MCP servers stay in place. Restart Claude Desktop afterwards so it forgets the connection.")
        }
        .onAppear {
            appState.refreshClaudeCodeStatus()
            refreshMcpStatus()
            Task { claudeIconPath = await ClaudeIconExtractor.tryExtract() }
        }
        .onDisappear {
            mcpRefreshTimer?.invalidate()
            mcpRefreshTimer = nil
        }
    }

    // MARK: - Summary card

    @ViewBuilder
    private var summaryCard: some View {
        HStack(alignment: .top, spacing: 14) {
            // Real Notion logo, bundled SVG (Providers/notion.imageset).
            // Rendered as a template so the monochrome mark tints to the
            // label colour: black in light, white in dark (the asset itself
            // is the black "original" mark, invisible on a dark surface).
            Image("notion")
                .renderingMode(.template)
                .resizable()
                .scaledToFit()
                .foregroundStyle(.primary)
                .frame(width: 40, height: 40)

            VStack(alignment: .leading, spacing: 6) {
                Text("Notion").font(.system(size: 16, weight: .semibold))
                Text(headerStatusText)
                    .font(.system(size: 13))
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)

                HStack(spacing: 8) {
                    if appState.hasNotionToken {
                        Button("Change destination") {
                            wizardInitialStep = 3
                            showWizard = true
                        }
                        Button("Disconnect") { showDisconnectConfirm = true }
                    } else {
                        Button("Connect Notion") {
                            wizardInitialStep = 1
                            showWizard = true
                        }
                        .buttonStyle(.borderedProminent)
                    }
                }
                .padding(.top, 4)
            }
            Spacer()
            Image(systemName: appState.hasNotionToken ? "checkmark.circle.fill" : "circle")
                .font(.system(size: 22))
                .foregroundStyle(appState.hasNotionToken ? .green : .secondary)
        }
        .padding(16)
        .background(
            RoundedRectangle(cornerRadius: 8)
                .fill(Color(NSColor.controlBackgroundColor))
        )
        .overlay(
            RoundedRectangle(cornerRadius: 8)
                .stroke(Color.gray.opacity(0.25), lineWidth: 1)
        )
    }

    private var headerStatusText: String {
        if appState.hasNotionToken {
            if !appState.notionTargetTitle.isEmpty {
                return "Connected. Recaps land in '\(appState.notionTargetTitle)'."
            }
            return "Connected · pick a destination"
        }
        return "Not connected"
    }

    // MARK: - Actions

    private func disconnect() {
        // Empty token = clear in keystore (notion.rs handles the
        // empty-string branch as "forget").
        _ = DimmyCore.shared.notionSetToken("")
        appState.hasNotionToken = false
        appState.notionTargetId = ""
        appState.notionTargetKind = ""
        appState.notionTargetTitle = ""
        appState.notionAutoSend = false
        // includeNotion: true, explicit clear intent. Generic
        // saves omit the target fields to avoid wiping a valid
        // destination accidentally; the disconnect path is one of
        // the only two sites that owns the clear semantics.
        DimmyCore.shared.setConfig(appState.toRustConfig(includeNotion: true))
        statusIsError = false
        statusMessage = "Disconnected. Token and destination removed from this device."
    }

    // MARK: - Claude Desktop MCP card

    private var mcpIsAlive: Bool {
        guard let age = mcpStatus.heartbeatAgeSecs else { return false }
        return age < 90
    }

    @ViewBuilder
    private var mcpCard: some View {
        HStack(alignment: .top, spacing: 14) {
            // Real Claude.app icon when extractable from /Applications;
            // otherwise the real Claude asterisk mark (ClaudeMark.imageset,
            // #D97757 burst), never an invented placeholder.
            // Cached under <configDir>/cache/claude-desktop-icon.png.
            if let p = claudeIconPath, let img = NSImage(contentsOfFile: p) {
                Image(nsImage: img)
                    .resizable()
                    .scaledToFit()
                    .frame(width: 40, height: 40)
            } else {
                Image("ClaudeMark")
                    .resizable()
                    .scaledToFit()
                    .frame(width: 36, height: 36)
                    .frame(width: 40, height: 40)
            }

            VStack(alignment: .leading, spacing: 6) {
                Text("Claude Desktop").font(.system(size: 16, weight: .semibold))
                Text(mcpHeaderText)
                    .font(.system(size: 13))
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)

                HStack(spacing: 8) {
                    if mcpStatus.extensionInstalled {
                        Button("Re-run wizard") { showMcpWizard = true }
                        Button("Disconnect") { showMcpDisconnectConfirm = true }
                        Button("Refresh") { refreshMcpStatus() }
                    } else {
                        Button("Connect Claude Desktop") { showMcpWizard = true }
                            .buttonStyle(.borderedProminent)
                    }
                }
                .padding(.top, 4)
            }
            Spacer()
            Image(systemName: mcpStatusIcon)
                .font(.system(size: 22))
                .foregroundStyle(mcpStatusColor)
        }
        .padding(16)
        .background(RoundedRectangle(cornerRadius: 8).fill(Color(NSColor.controlBackgroundColor)))
        .overlay(RoundedRectangle(cornerRadius: 8).stroke(Color.gray.opacity(0.25), lineWidth: 1))
    }

    private var mcpHeaderText: String {
        if mcpIsAlive {
            if let tool = mcpStatus.lastCallTool {
                return "Connected · last call \(mcpStatus.lastCallAgoSecs ?? 0)s ago (\(tool))"
            }
            return "Connected · heartbeat \(mcpStatus.heartbeatAgeSecs ?? 0)s ago"
        }
        if mcpStatus.extensionInstalled {
            // Heartbeat ages out fast (Claude kills idle MCP servers
            // by design). Keep the wording neutral, not an error.
            if let v = mcpStatus.extensionVersion {
                return "Installed (v\(v)) · Claude spawns on demand"
            }
            return "Installed · Claude spawns on demand"
        }
        return mcpStatus.installed
            ? "Not connected to Claude Desktop yet"
            : "Claude Desktop isn't installed on this Mac"
    }

    private var mcpStatusIcon: String {
        if mcpIsAlive { return "checkmark.circle.fill" }
        if mcpStatus.extensionInstalled { return "checkmark.circle.fill" }
        return "circle"
    }

    private var mcpStatusColor: Color {
        if mcpIsAlive { return .green }
        if mcpStatus.extensionInstalled { return .green }
        return .secondary
    }

    private func refreshMcpStatus() {
        mcpStatus = DimmyCore.shared.claudeDesktopStatus()
    }
}

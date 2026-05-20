import SwiftUI

/// Settings → Integrations page. Mac mirror of the Win
/// `IntegrationsPanel` — summary card + 3-step sheet wizard
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

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            MacGroupLabel(text: "Anthropic — Claude Code subscription")
            // Detailed status + sign-in card. Sign-in opens the
            // browser via `claude /login` running in Terminal — the
            // token is stored either in `~/.claude/credentials.json`
            // or (recent CLIs) macOS Keychain under service
            // "Claude Code-credentials". Dimmy never touches the
            // token directly; only probes existence.
            MacTile {
                MacClaudeCodeCard(
                    appState: appState,
                    onWizardRequested: { showClaudeWizard = true }
                )
            }
            MacGroupFooter(text: "Sign-in opens Anthropic's OAuth in your browser via the local `claude` CLI. The OAuth token is stored by the CLI in macOS Keychain (or ~/.claude/credentials.json on older versions). Dimmy reads only the existence of the token — never its contents.")

            Spacer().frame(height: 24)
            MacGroupLabel(text: "Notion")

            // Summary card — current state + action buttons.
            summaryCard

            // Auto-send toggle — only meaningful once connected.
            // Stays inline (not in wizard) because users may flip it
            // on/off over time without re-entering setup.
            Spacer().frame(height: 16)
            MacGroupLabel(text: "Automation")
            MacTile {
                MacRow(
                    "Auto-send each meeting",
                    description: "When on, every meeting's recap uploads to your Notion destination as soon as the recap finishes. When off, you click Send to Notion from the meeting Done view per meeting.",
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
            Text("Free Notion accounts work — no API limits per plan, only the standard 3 requests/sec rate limit. Token + destination stay on this device; only the recap markdown leaves when you (or auto-send) trigger an upload.")
                .font(.system(size: 11))
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
        .sheet(isPresented: $showWizard) {
            NotionConnectSheet(
                appState: appState,
                initialStep: wizardInitialStep,
                onClose: {
                    showWizard = false
                    if appState.hasNotionToken && !appState.notionTargetTitle.isEmpty {
                        statusIsError = false
                        statusMessage = "Recaps will land in “\(appState.notionTargetTitle)”."
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
        .onAppear {
            // Re-probe Claude Code status so the Anthropic card above
            // reflects the live install + Keychain state. Cheap call
            // (file stat + a `security find-generic-password` shell-out).
            appState.refreshClaudeCodeStatus()
        }
    }

    // MARK: - Summary card

    @ViewBuilder
    private var summaryCard: some View {
        HStack(alignment: .top, spacing: 14) {
            // Real Notion logo — bundled SVG asset (Providers/notion.imageset).
            Image("notion")
                .resizable()
                .scaledToFit()
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
                return "Connected · recaps land in “\(appState.notionTargetTitle)”"
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
        // includeNotion: true — explicit clear intent. Generic
        // saves omit the target fields to avoid wiping a valid
        // destination accidentally; the disconnect path is one of
        // the only two sites that owns the clear semantics.
        DimmyCore.shared.setConfig(appState.toRustConfig(includeNotion: true))
        statusIsError = false
        statusMessage = "Disconnected. Token and destination removed from this device."
    }
}

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

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
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
        DimmyCore.shared.setConfig(appState.toRustConfig())
        statusIsError = false
        statusMessage = "Disconnected. Token and destination removed from this device."
    }
}

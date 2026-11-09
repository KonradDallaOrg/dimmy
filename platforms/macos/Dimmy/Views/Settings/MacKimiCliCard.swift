import SwiftUI

// MARK: - MacKimiCliCard
//
// Kimi membership card, sibling of MacCodexCard, driven by the dimmy_kimi_*
// FFI. Three states (ready / not signed in / not installed). There is no
// "use for rewrite" toggle here: the Output page's Authentication picker
// turns the subscription on for a Kimi Code model, as on Windows.
//
// Privacy: Dimmy never reads ~/.kimi-code/credentials; the CLI owns the token.

struct MacKimiCliCard: View {
    @ObservedObject var appState: AppState

    /// Opens the setup wizard. Optional so the card works standalone.
    var onWizardRequested: (() -> Void)? = nil

    @State private var status: DimmyCore.ClaudeCodeStatus = .notInstalled
    @State private var binaryPath: String? = nil
    @State private var signInRunning = false
    @State private var testRunning = false
    @State private var statusMessage = ""
    @State private var mcpFound = false
    @State private var mcpRegistered = false
    @State private var mcpMessage = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(alignment: .top, spacing: 14) {
                Image("kimi")
                    .resizable()
                    .scaledToFit()
                    .frame(width: 30, height: 30)
                    .frame(width: 40, height: 40)

                VStack(alignment: .leading, spacing: 6) {
                    Text("Kimi Code subscription").font(.system(size: 16, weight: .semibold))
                    Text(descriptionText)
                        .font(.system(size: 13))
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)

                    HStack(spacing: 8) {
                        if signInRunning || testRunning {
                            ProgressView().controlSize(.small).scaleEffect(0.7)
                        }
                        if let openWizard = onWizardRequested {
                            if status == .notInstalled {
                                Button(action: openWizard) {
                                    Label("Set up wizard", systemImage: "wand.and.stars")
                                }
                                .controlSize(.small)
                                .buttonStyle(.borderedProminent)
                                .help("Guided install + Kimi sign-in, one click per step")
                            } else {
                                Button(action: openWizard) {
                                    Label("Re-run wizard", systemImage: "wand.and.stars")
                                }
                                .controlSize(.small)
                            }
                        }
                        Button(action: signIn) {
                            Label(status == .ready ? "Re-sign in" : "Sign in with Kimi",
                                  systemImage: "person.badge.key.fill")
                        }
                        .controlSize(.small)
                        .disabled(signInRunning || testRunning || status == .notInstalled)
                        Button(action: testConnection) {
                            Label("Test", systemImage: "checkmark.circle.fill")
                        }
                        .controlSize(.small)
                        .disabled(signInRunning || testRunning || status != .ready)
                        .help("Send a small ping prompt through `kimi acp` (binary + login + plan)")
                        Button(action: refresh) {
                            Image(systemName: "arrow.clockwise")
                        }
                        .controlSize(.small)
                        .help("Re-probe Kimi Code status")
                    }
                    .padding(.top, 4)
                }
                Spacer()
                Image(systemName: statusIconName)
                    .font(.system(size: 22))
                    .foregroundStyle(statusIconColor)
            }

            // Kimi Code MCP bridge: Kimi Code (CLI and Desktop) reads
            // Dimmy's meetings through dimmy-mcp. Shown once Kimi is here.
            if mcpFound {
                Toggle(isOn: Binding(get: { mcpRegistered }, set: { setMcp($0) })) {
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Let Kimi Code read your meetings").font(.system(size: 13))
                        Text(mcpMessage.isEmpty
                             ? "Adds Dimmy to ~/.kimi-code/mcp.json, for Kimi Code CLI and Kimi Code Desktop. Other entries stay as they are."
                             : mcpMessage)
                            .font(.system(size: 12))
                            .foregroundStyle(.secondary)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
                .toggleStyle(.switch)
                .controlSize(.small)
            }
        }
        .padding(16)
        .background(
            RoundedRectangle(cornerRadius: 8).fill(Color(NSColor.controlBackgroundColor))
        )
        .overlay(
            RoundedRectangle(cornerRadius: 8).stroke(Color.gray.opacity(0.25), lineWidth: 1)
        )
        .onAppear { refresh() }
    }

    private var descriptionText: String {
        if !statusMessage.isEmpty { return statusMessage }
        switch status {
        case .ready:
            if let p = binaryPath { return "✓ Signed in. Using `kimi` at \(p)." }
            return "✓ Signed in with your Kimi plan."
        case .notLoggedIn:
            return "Kimi Code installed but not signed in. Click Sign in to use your Kimi account."
        case .notInstalled:
            return "Kimi Code CLI not detected. Click Set up wizard: it installs everything for you."
        }
    }

    private var statusIconName: String {
        switch status {
        case .ready: return "checkmark.circle.fill"
        case .notLoggedIn: return "exclamationmark.triangle.fill"
        case .notInstalled: return "circle"
        }
    }

    private var statusIconColor: Color {
        switch status {
        case .ready: return .green
        case .notLoggedIn: return .orange
        case .notInstalled: return .secondary
        }
    }

    private func refresh() {
        status = DimmyCore.shared.recheckKimi()
        binaryPath = DimmyCore.shared.kimiBinaryPath
        statusMessage = ""
        appState.refreshKimiStatus()
        refreshMcp()
    }

    private func refreshMcp() {
        let s = DimmyCore.shared.kimiMcpStatus
        mcpFound = s.kimiFound
        mcpRegistered = s.registered
    }

    private func setMcp(_ on: Bool) {
        if on {
            let bin = Bundle.main.bundlePath + "/Contents/Resources/dimmy-mcp"
            let rc: Int32 = FileManager.default.fileExists(atPath: bin)
                ? DimmyCore.shared.installKimiMcp(binaryPath: bin) : -2
            switch rc {
            case 0: mcpMessage = "Connected. Open a new Kimi Code session to use Dimmy's tools."
            case -2: mcpMessage = "dimmy-mcp was not found in the app bundle."
            case -3: mcpMessage = "~/.kimi-code/mcp.json is not valid JSON. Dimmy left it untouched; fix it and try again."
            default: mcpMessage = "Could not write ~/.kimi-code/mcp.json. See dimmy.log."
            }
        } else {
            let rc = DimmyCore.shared.uninstallKimiMcp()
            mcpMessage = rc < 0 ? "Could not update ~/.kimi-code/mcp.json. See dimmy.log." : ""
        }
        refreshMcp()
    }

    private func signIn() {
        signInRunning = true
        statusMessage = "Launching `kimi login` in Terminal, confirm the code in your browser."
        DispatchQueue.global(qos: .userInitiated).async {
            let ok = DimmyCore.shared.spawnKimiLogin()
            DispatchQueue.main.async {
                if !ok {
                    signInRunning = false
                    statusMessage = "Could not start `kimi login`. Open Terminal and run it manually."
                    DimmyCore.shared.trackEvent("kimi_cli.login_completed", ["outcome": "spawn_failed"])
                    return
                }
                pollForCompletion(attempt: 0, max: 90)
            }
        }
    }

    /// Documented polling exception: the sign-in happens in another process.
    private func pollForCompletion(attempt: Int, max: Int) {
        if attempt >= max {
            signInRunning = false
            statusMessage = "Sign-in not completed in 3 minutes. Click ↻ when ready."
            DimmyCore.shared.trackEvent("kimi_cli.login_completed", ["outcome": "timeout"])
            return
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + 2.0) {
            DispatchQueue.global(qos: .userInitiated).async {
                let s = DimmyCore.shared.recheckKimi()
                DispatchQueue.main.async {
                    if s == .ready {
                        signInRunning = false
                        DimmyCore.shared.trackEvent("kimi_cli.login_completed", ["outcome": "success"])
                        refresh()
                    } else {
                        pollForCompletion(attempt: attempt + 1, max: max)
                    }
                }
            }
        }
    }

    private func testConnection() {
        testRunning = true
        statusMessage = "Sending ping..."
        DispatchQueue.global(qos: .userInitiated).async {
            let result = DimmyCore.shared.pingKimi()
            DispatchQueue.main.async {
                testRunning = false
                statusMessage = KimiConnectSheet.describe(result)
            }
        }
    }
}

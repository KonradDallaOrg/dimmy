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

    var body: some View {
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

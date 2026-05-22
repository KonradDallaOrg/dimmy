import SwiftUI

// MARK: - MacClaudeCodeCard
//
// Mirrors the Win SettingsWindow `ClaudeCodeStatusCard`. Shown only
// when the user has picked the "Claude Code (subscription)" LLM
// preset (`llmApiUrl` starts with `claude-code://`).
//
// Three states drive the labels + buttons:
//   - `.ready`        ✓ Logged in + binary path shown. "Re-sign in"
//                     button available for re-auth.
//   - `.notLoggedIn`  Binary detected but credentials missing →
//                     "Sign in via browser" button spawns
//                     `claude /login` in a new Terminal window via
//                     osascript. Polls status every 2 s for 3 min.
//   - `.notInstalled` Binary missing → install hint + manual ↻
//                     button. Button is disabled until the user
//                     installs the CLI (we have no way to install
//                     for them).
//
// Privacy: Dimmy never touches `~/.claude/credentials.json`. The
// CLI is the only consumer of the token; we just observe whether
// the file exists + length > 0 via the Rust core.

struct MacClaudeCodeCard: View {
    @ObservedObject var appState: AppState
    /// Called when the user clicks "Set up wizard" (shown only when
    /// the CLI is missing). Parent owns the sheet state.
    var onWizardRequested: (() -> Void)? = nil

    @State private var status: DimmyCore.ClaudeCodeStatus = .notInstalled
    @State private var binaryPath: String? = nil
    @State private var signInRunning: Bool = false
    @State private var testRunning: Bool = false
    @State private var statusMessage: String = ""

    var body: some View {
        // Hand-rolled HStack so the card matches the Notion + MCP
        // cards lower on this page (icon left, content + buttons
        // middle, status badge top-right, all with the same padding
        // + spacing). Using MacRow + an overlay produced a check
        // that collided with the action buttons — see git history.
        HStack(alignment: .top, spacing: 14) {
            // Brand icon — orange Anthropic-style squircle.
            ZStack {
                RoundedRectangle(cornerRadius: 9)
                    .fill(Color(red: 0.84, green: 0.47, blue: 0.21))
                Image(systemName: "person.crop.circle.badge.checkmark")
                    .font(.system(size: 22, weight: .semibold))
                    .foregroundColor(.white)
            }
            .frame(width: 40, height: 40)

            VStack(alignment: .leading, spacing: 6) {
                Text("Claude Code subscription").font(.system(size: 16, weight: .semibold))
                Text(descriptionText)
                    .font(.system(size: 13))
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)

                HStack(spacing: 8) {
                    if signInRunning || testRunning {
                        ProgressView().controlSize(.small).scaleEffect(0.7)
                    }
                    if status == .notInstalled, onWizardRequested != nil {
                        Button(action: { onWizardRequested?() }) {
                            Label("Set up wizard", systemImage: "wand.and.stars")
                        }
                        .controlSize(.small)
                        .buttonStyle(.borderedProminent)
                    } else {
                        Button(action: signIn) {
                            Label(signInLabel, systemImage: "person.badge.key.fill")
                        }
                        .controlSize(.small)
                        .disabled(signInDisabled)
                    }
                    Button(action: testConnection) {
                        Label("Test", systemImage: "checkmark.circle.fill")
                    }
                    .controlSize(.small)
                    .disabled(testDisabled)
                    .help("Send a small ping prompt to verify end-to-end (binary + login + network)")
                    Button(action: refresh) {
                        Image(systemName: "arrow.clockwise")
                    }
                    .controlSize(.small)
                    .help("Re-probe Claude Code status")
                }
                .padding(.top, 4)
            }
            Spacer()
            Image(systemName: statusIconName)
                .font(.system(size: 22))
                .foregroundStyle(statusIconColor)
                .help(statusIconHelp)
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
            if let p = binaryPath {
                return "✓ Logged in. Using `claude` at \(p)."
            }
            return "✓ Logged in via Anthropic Pro / Team / Max."
        case .notLoggedIn:
            return "Claude Code installed but not logged in. Click Sign in to authenticate via browser."
        case .notInstalled:
            return "Claude Code CLI not detected. Install via `brew install claude` or from anthropic.com, then click ↻."
        }
    }

    private var signInLabel: String {
        switch status {
        case .ready: return "Re-sign in"
        case .notLoggedIn: return "Sign in via browser"
        case .notInstalled: return "Install Claude Code first"
        }
    }

    private var signInDisabled: Bool {
        signInRunning || testRunning || status == .notInstalled
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

    private var statusIconHelp: String {
        switch status {
        case .ready: return "Connected"
        case .notLoggedIn: return "Installed but not signed in"
        case .notInstalled: return "Not installed"
        }
    }

    private var testDisabled: Bool {
        signInRunning || testRunning || status != .ready
    }

    private func refresh() {
        status = DimmyCore.shared.claudeCodeStatus
        binaryPath = DimmyCore.shared.claudeCodeBinaryPath
        statusMessage = ""
    }

    private func signIn() {
        signInRunning = true
        statusMessage = "Launching `claude /login` in Terminal — complete the browser flow there."
        DispatchQueue.global(qos: .userInitiated).async {
            let ok = DimmyCore.shared.spawnClaudeCodeLogin()
            DispatchQueue.main.async {
                if !ok {
                    signInRunning = false
                    statusMessage = "Could not spawn `claude /login`. Open Terminal and run it manually."
                    DimmyCore.shared.trackEvent(
                        "claude_code.login_completed",
                        ["outcome": "spawn_failed"])
                    return
                }
                // Poll for up to 3 minutes (90 × 2 s). Matches the
                // Win polling cadence. The Rust side has no
                // completion callback so polling is the only signal.
                pollForCompletion(attempt: 0, max: 90)
            }
        }
    }

    private func pollForCompletion(attempt: Int, max: Int) {
        if attempt >= max {
            signInRunning = false
            statusMessage = "Sign-in not completed in 3 minutes. Click ↻ when ready."
            DimmyCore.shared.trackEvent(
                "claude_code.login_completed",
                ["outcome": "timeout"])
            return
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + 2.0) {
            let s = DimmyCore.shared.claudeCodeStatus
            if s == .ready {
                signInRunning = false
                statusMessage = ""
                DimmyCore.shared.trackEvent(
                    "claude_code.login_completed",
                    ["outcome": "success"])
                refresh()
                return
            }
            pollForCompletion(attempt: attempt + 1, max: max)
        }
    }

    private func testConnection() {
        testRunning = true
        statusMessage = "Sending ping…"
        DispatchQueue.global(qos: .userInitiated).async {
            let result = DimmyCore.shared.pingClaudeCode()
            DispatchQueue.main.async {
                testRunning = false
                statusMessage = describe(result)
            }
        }
    }

    private func describe(_ r: DimmyCore.ClaudeCodePingResult) -> String {
        switch r {
        case .ok(let ms): return "✓ Connection OK (\(ms) ms round-trip)"
        case .notInstalled: return "✗ `claude` binary not found. Install Claude Code first."
        case .notLoggedIn: return "✗ Not logged in. Click Sign in to authenticate."
        case .spawnFailed: return "✗ Could not spawn the CLI. See ~/Library/Logs/Dimmy/dimmy.log."
        case .timeout: return "✗ Timed out after 15 s — network or rate-limit issue."
        case .nonZeroExit: return "✗ `claude` returned a non-zero exit code. See dimmy.log."
        case .invalidUtf8: return "✗ Unexpected output from `claude`. See dimmy.log."
        case .unknownError: return "✗ Unknown error. See dimmy.log."
        }
    }
}

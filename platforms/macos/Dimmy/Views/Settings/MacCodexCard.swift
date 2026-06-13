import SwiftUI

// MARK: - MacCodexCard
//
// OpenAI / ChatGPT subscription card — direct mirror of
// MacClaudeCodeCard, driven by the dimmy_codex_* FFI. Use the user's
// ChatGPT Plus / Pro / Team plan via the local `codex` CLI instead of
// API keys. Three states (ready / not signed in / not installed) plus a
// "Use for recap + rewrite" toggle that points llm_api_url at
// codex://default so both the LLM rewrite and the recap that follows it
// route through Codex. Reversible — off restores the previous provider.
//
// Privacy: Dimmy never reads ~/.codex/auth.json; the CLI owns the token.

struct MacCodexCard: View {
    @ObservedObject var appState: AppState

    @State private var status: DimmyCore.ClaudeCodeStatus = .notInstalled
    @State private var binaryPath: String? = nil
    @State private var signInRunning: Bool = false
    @State private var testRunning: Bool = false
    @State private var statusMessage: String = ""
    @State private var llmUrlBeforeCodex: String = ""

    private let codexUrl = "codex://default"

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(alignment: .top, spacing: 14) {
                Image(systemName: "bubble.left.and.text.bubble.right.fill")
                    .font(.system(size: 26))
                    .foregroundStyle(.secondary)
                    .frame(width: 40, height: 40)

                VStack(alignment: .leading, spacing: 6) {
                    Text("ChatGPT (Codex) subscription").font(.system(size: 16, weight: .semibold))
                    Text(descriptionText)
                        .font(.system(size: 13))
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)

                    HStack(spacing: 8) {
                        if signInRunning || testRunning {
                            ProgressView().controlSize(.small).scaleEffect(0.7)
                        }
                        Button(action: signIn) {
                            Label(signInLabel, systemImage: "person.badge.key.fill")
                        }
                        .controlSize(.small)
                        .disabled(signInDisabled)
                        Button(action: testConnection) {
                            Label("Test", systemImage: "checkmark.circle.fill")
                        }
                        .controlSize(.small)
                        .disabled(testDisabled)
                        .help("Send a small ping prompt to verify end-to-end (binary + login + ChatGPT)")
                        Button(action: refresh) {
                            Image(systemName: "arrow.clockwise")
                        }
                        .controlSize(.small)
                        .help("Re-probe Codex status")
                    }
                    .padding(.top, 4)
                }
                Spacer()
                Image(systemName: statusIconName)
                    .font(.system(size: 22))
                    .foregroundStyle(statusIconColor)
                    .help(statusIconHelp)
            }

            if status == .ready {
                Toggle(isOn: Binding(
                    get: { appState.llmApiUrl == codexUrl },
                    set: { useCodex in setUseCodex(useCodex) }
                )) {
                    Text("Use ChatGPT (Codex) for recap and rewrite")
                        .font(.system(size: 13))
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
            if let p = binaryPath { return "✓ Signed in. Using `codex` at \(p)." }
            return "✓ Signed in with your ChatGPT plan."
        case .notLoggedIn:
            return "Codex installed but not signed in. Click Sign in to authenticate with ChatGPT."
        case .notInstalled:
            return "Codex CLI not detected. Install via `npm install -g @openai/codex` (or the native installer), then click ↻."
        }
    }

    private var signInLabel: String {
        switch status {
        case .ready: return "Re-sign in"
        case .notLoggedIn: return "Sign in with ChatGPT"
        case .notInstalled: return "Install Codex first"
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
        status = DimmyCore.shared.codexStatus
        binaryPath = DimmyCore.shared.codexBinaryPath
        statusMessage = ""
    }

    /// Point the LLM (rewrite + recap that follows it) at codex://default,
    /// or restore the previous provider. Reversible. Persists via the live
    /// config write.
    private func setUseCodex(_ useCodex: Bool) {
        if useCodex {
            if appState.llmApiUrl != codexUrl { llmUrlBeforeCodex = appState.llmApiUrl }
            appState.llmApiUrl = codexUrl
        } else {
            appState.llmApiUrl = llmUrlBeforeCodex
        }
        DimmyCore.shared.setConfig(appState.toRustConfig())
    }

    private func signIn() {
        signInRunning = true
        statusMessage = "Launching `codex login` in Terminal, complete the ChatGPT sign-in there."
        DispatchQueue.global(qos: .userInitiated).async {
            let ok = DimmyCore.shared.spawnCodexLogin()
            DispatchQueue.main.async {
                if !ok {
                    signInRunning = false
                    statusMessage = "Could not spawn `codex login`. Open Terminal and run it manually."
                    DimmyCore.shared.trackEvent("codex.login_completed", ["outcome": "spawn_failed"])
                    return
                }
                pollForCompletion(attempt: 0, max: 90)
            }
        }
    }

    private func pollForCompletion(attempt: Int, max: Int) {
        if attempt >= max {
            signInRunning = false
            statusMessage = "Sign-in not completed in 3 minutes. Click ↻ when ready."
            DimmyCore.shared.trackEvent("codex.login_completed", ["outcome": "timeout"])
            return
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + 2.0) {
            if DimmyCore.shared.codexStatus == .ready {
                signInRunning = false
                statusMessage = ""
                DimmyCore.shared.trackEvent("codex.login_completed", ["outcome": "success"])
                refresh()
                return
            }
            pollForCompletion(attempt: attempt + 1, max: max)
        }
    }

    private func testConnection() {
        testRunning = true
        statusMessage = "Sending ping..."
        DispatchQueue.global(qos: .userInitiated).async {
            let result = DimmyCore.shared.pingCodex()
            DispatchQueue.main.async {
                testRunning = false
                statusMessage = describe(result)
            }
        }
    }

    private func describe(_ r: DimmyCore.ClaudeCodePingResult) -> String {
        switch r {
        case .ok(let ms): return "✓ Connection OK (\(ms) ms round-trip)"
        case .notInstalled: return "✗ `codex` binary not found. Install the Codex CLI first."
        case .notLoggedIn: return "✗ Not signed in. Click Sign in to authenticate with ChatGPT."
        case .spawnFailed: return "✗ Could not spawn the CLI. See ~/Library/Logs/Dimmy/dimmy.log."
        case .timeout: return "✗ Timed out after 30 s, network or rate-limit issue."
        case .nonZeroExit: return "✗ `codex` returned a non-zero exit code. See dimmy.log."
        case .invalidUtf8: return "✗ Unexpected output from `codex`. See dimmy.log."
        case .unknownError: return "✗ Unknown error. See dimmy.log."
        }
    }
}

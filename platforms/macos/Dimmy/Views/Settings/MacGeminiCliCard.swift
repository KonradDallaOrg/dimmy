import SwiftUI

// MARK: - MacGeminiCliCard
//
// Google (Gemini CLI) integration card. Same anatomy as MacCodexCard and
// MacClaudeCodeCard, with one thing neither of them has: a GATE.
//
// Google ended Gemini CLI access for personal accounts on 2026-06-18, Google
// AI Pro and Ultra included. The only accounts it still serves are Gemini
// Code Assist Standard / Enterprise company seats. Ungated, this card would
// walk an ordinary user through an install and a sign-in that cannot
// succeed, and the refusal arrives at the very end in Google's words.
//
// So the toggle comes first and everything else is hidden behind it. The
// three sentences explaining who this is for live behind an (i), not under
// the title: they matter to the few people who reach for this and to nobody
// else.

struct MacGeminiCliCard: View {
    @ObservedObject var appState: AppState

    /// Opens the setup wizard. Optional so the card works standalone;
    /// MacIntegrationsPage passes it.
    var onWizardRequested: (() -> Void)? = nil

    @State private var status: DimmyCore.ClaudeCodeStatus = .notInstalled
    @State private var binaryPath: String? = nil
    @State private var signInRunning: Bool = false
    @State private var testRunning: Bool = false
    @State private var statusMessage: String = ""
    @State private var authBeforeGemini: String = "api_key"
    @State private var showingInfo: Bool = false

    /// The real Gemini endpoint. NOT the synthetic `gemini-cli://` scheme:
    /// macOS moved off synthetic LLM URLs on 2026-06-20 and migrates the old
    /// ones away on load, because a sentinel URL forced the model picker to
    /// jump every time the user touched Authentication. The subscription
    /// signal here is `llm_auth_method`, and the Rust dispatcher routes
    /// (gemini url + "subscription") to the CLI exactly as it does for the
    /// other two backends.
    private let geminiUrl = "https://generativelanguage.googleapis.com/v1beta/models"

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(alignment: .top, spacing: 14) {
                // The real Gemini spark (Assets/Providers/gemini.imageset).
                // NOT template-rendered, unlike the OpenAI and Anthropic
                // marks: this one carries its own brand gradient and tinting
                // it to the label colour would throw the gradient away.
                Image("gemini")
                    .resizable()
                    .scaledToFit()
                    .frame(width: 30, height: 30)
                    .frame(width: 40, height: 40)

                VStack(alignment: .leading, spacing: 6) {
                    HStack(spacing: 6) {
                        Text("Gemini (Google account)")
                            .font(.system(size: 16, weight: .semibold))
                        // The caveat is three sentences and concerns only the
                        // handful of people who can use this at all.
                        Button { showingInfo.toggle() } label: {
                            Image(systemName: "info.circle")
                                .foregroundStyle(.secondary)
                        }
                        .buttonStyle(.plain)
                        .popover(isPresented: $showingInfo, arrowEdge: .bottom) {
                            VStack(alignment: .leading, spacing: 8) {
                                Text("Turn this on only if your organisation gives you Gemini Code Assist Standard or Enterprise.")
                                Text("Check with whoever administers it first: that seat is governed by your organisation's Google Cloud agreement, not by us.")
                                    .foregroundStyle(.secondary)
                                Text("Personal Google accounts, including AI Pro and Ultra, lost access to this CLI on 18 June 2026 and will be refused at sign-in.")
                                    .foregroundStyle(.secondary)
                            }
                            .font(.system(size: 13))
                            .frame(width: 320)
                            .padding(14)
                        }
                        .help("Who can use this")
                    }

                    Text(descriptionText)
                        .font(.system(size: 13))
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)

                    if appState.geminiCliEnabled {
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
                                    .help("Guided install + Google sign-in walkthrough")
                                } else {
                                    Button(action: openWizard) {
                                        Label("Re-run wizard", systemImage: "wand.and.stars")
                                    }
                                    .controlSize(.small)
                                    .help("Walk through install + sign-in again from scratch")
                                }
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
                            .help("Send a small ping prompt to verify end-to-end (binary + sign-in + quota)")
                            Button(action: refresh) {
                                Image(systemName: "arrow.clockwise")
                            }
                            .controlSize(.small)
                            .help("Re-probe the local `gemini` CLI")
                        }
                        .padding(.top, 4)
                    }
                }
                Spacer()
                Image(systemName: statusIconName)
                    .font(.system(size: 22))
                    .foregroundStyle(statusIconColor)
                    .help(statusIconHelp)
            }

            // The gate itself. Always visible, so it can be turned back off.
            Toggle(isOn: Binding(
                get: { appState.geminiCliEnabled },
                set: { setEnabled($0) }
            )) {
                Text("I have a Gemini Code Assist work account")
                    .font(.system(size: 13))
            }
            .toggleStyle(.switch)
            .controlSize(.small)

            if appState.geminiCliEnabled && status == .ready {
                Toggle(isOn: Binding(
                    get: { usingGemini },
                    set: { useGemini in setUseGemini(useGemini) }
                )) {
                    Text("Use Gemini for recap and rewrite")
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
        if !appState.geminiCliEnabled {
            return "Off. Available for Gemini Code Assist work accounts."
        }
        if !statusMessage.isEmpty { return statusMessage }
        switch status {
        case .ready:
            if let p = binaryPath { return "✓ Signed in. Using `gemini` at \(p)." }
            return "✓ Signed in with your Google account."
        case .notLoggedIn:
            return "Gemini CLI installed but not signed in. Click Sign in to authenticate with Google."
        case .notInstalled:
            return "Gemini CLI not detected. Install via `npm install -g @google/gemini-cli` (needs Node 20+), then click ↻."
        }
    }

    private var signInLabel: String {
        switch status {
        case .ready: return "Re-sign in"
        case .notLoggedIn: return "Sign in with Google"
        case .notInstalled: return "Install the CLI first"
        }
    }

    private var signInDisabled: Bool {
        signInRunning || testRunning || status == .notInstalled
    }

    private var statusIconName: String {
        guard appState.geminiCliEnabled else { return "minus.circle" }
        switch status {
        case .ready: return "checkmark.circle.fill"
        case .notLoggedIn: return "exclamationmark.triangle.fill"
        case .notInstalled: return "circle"
        }
    }

    private var statusIconColor: Color {
        guard appState.geminiCliEnabled else { return .secondary }
        switch status {
        case .ready: return .green
        case .notLoggedIn: return .orange
        case .notInstalled: return .secondary
        }
    }

    private var statusIconHelp: String {
        guard appState.geminiCliEnabled else { return "Off" }
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
        guard appState.geminiCliEnabled else { return }
        status = DimmyCore.shared.geminiCliStatus
        binaryPath = DimmyCore.shared.geminiCliBinaryPath
        statusMessage = ""
    }

    private func setEnabled(_ on: Bool) {
        appState.geminiCliEnabled = on
        DimmyCore.shared.setConfig(appState.toRustConfig())
        if on { refresh() }
    }

    /// True when rewrite + the recap that follows it are already routed
    /// through the CLI: a Gemini endpoint AND subscription auth. Both halves
    /// matter — a Gemini URL with an API key is the ordinary HTTP provider.
    private var usingGemini: Bool {
        appState.llmAuthMethod == "subscription"
            && appState.llmApiUrl.contains("generativelanguage.googleapis.com")
    }

    /// Route the LLM through the CLI, or put it back. Reversible: the
    /// previous auth method is remembered rather than assumed to be
    /// "api_key".
    private func setUseGemini(_ useGemini: Bool) {
        if useGemini {
            if !usingGemini { authBeforeGemini = appState.llmAuthMethod }
            appState.llmApiUrl = geminiUrl
            appState.llmAuthMethod = "subscription"
        } else {
            appState.llmAuthMethod = authBeforeGemini
        }
        DimmyCore.shared.setConfig(appState.toRustConfig())
    }

    private func signIn() {
        signInRunning = true
        statusMessage = "Launching the Gemini CLI in Terminal. Choose 'Login with Google' there."
        DispatchQueue.global(qos: .userInitiated).async {
            let ok = DimmyCore.shared.spawnGeminiCliLogin()
            DispatchQueue.main.async {
                if !ok {
                    signInRunning = false
                    statusMessage = "Could not launch the CLI. Open Terminal and run `gemini` manually."
                    DimmyCore.shared.trackEvent("gemini_cli.login_completed", ["outcome": "spawn_failed"])
                    return
                }
                pollForCompletion(attempt: 0, max: 90)
            }
        }
    }

    /// Poll rather than wait on the process: the CLI stays open after the
    /// browser flow completes (it drops into its own prompt), so its exit is
    /// not the signal we want — the credentials file is.
    private func pollForCompletion(attempt: Int, max: Int) {
        if attempt >= max {
            signInRunning = false
            statusMessage = "Sign-in not completed in 3 minutes. Click ↻ when ready."
            DimmyCore.shared.trackEvent("gemini_cli.login_completed", ["outcome": "timeout"])
            return
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + 2.0) {
            if DimmyCore.shared.recheckGeminiCli() == .ready {
                signInRunning = false
                statusMessage = ""
                DimmyCore.shared.trackEvent("gemini_cli.login_completed", ["outcome": "success"])
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
            let result = DimmyCore.shared.pingGeminiCli()
            let reported = DimmyCore.shared.geminiCliLastError
            DispatchQueue.main.async {
                testRunning = false
                statusMessage = describe(result, reported: reported)
            }
        }
    }

    private func describe(_ r: DimmyCore.GeminiCliPingResult, reported: String) -> String {
        switch r {
        case .ok(let ms): return "✓ Connection OK (\(ms) ms round-trip)"
        case .notInstalled: return "✗ `gemini` binary not found. Install the Gemini CLI first."
        case .notSignedIn: return "✗ Not signed in. Click Sign in to authenticate with Google."
        case .spawnFailed: return "✗ Could not launch the CLI. See ~/Library/Logs/Dimmy/dimmy.log."
        case .timeout: return "✗ Timed out after 60 s — network or rate-limit issue."
        case .nonZeroExit: return "✗ `gemini` exited non-zero. See ~/Library/Logs/Dimmy/dimmy.log."
        case .invalidUtf8: return "✗ Unexpected output from `gemini`."
        // Google's own words: the usual cause is a personal account being
        // refused, or the day's requests being used up, and both are things
        // the user can act on only if they can read them.
        case .reported:
            return reported.isEmpty ? "✗ Gemini refused the request." : "✗ \(reported)"
        case .emptyResponse: return "✗ Gemini answered with nothing. Try again, or check your quota."
        case .unknownError: return "✗ Unknown error. See ~/Library/Logs/Dimmy/dimmy.log."
        }
    }
}

import SwiftUI
import AppKit

/// 3-step Claude Code subscription setup wizard, modal sheet.
/// Mac mirror of `Views/ClaudeConnectDialog.xaml` on Windows.
///
/// Flow:
///   Step 1 — Detect / install Node.js (≥ 18 required).
///   Step 2 — Detect / install Claude CLI via `npm install -g`.
///   Step 3 — Sign in (browser OAuth via `claude /login`) +
///            auto-fire test ping.
///
/// Smart-skip: on appearance we probe each precondition once and
/// jump to the first incomplete step. A machine where everything is
/// already set lands on Step 3 with test-ready state.
///
/// Completion contract: `onComplete` is called with the wizard
/// outcome — `true` iff Step 3's test ping returned positive. The
/// caller (MacIntegrationsPage) then flips
/// `llm_auth_method=subscription` so the integration is live without
/// a second click.
struct ClaudeConnectSheet: View {
    @ObservedObject var appState: AppState
    let onClose: () -> Void
    let onComplete: (Bool) -> Void

    private enum WizardStep: Int { case node = 1, claudeCli = 2, signIn = 3 }

    @State private var currentStep: WizardStep = .node
    @State private var nodeStatus: DimmyCore.NodeStatus = .missing
    @State private var claudeStatus: DimmyCore.ClaudeCodeStatus = .notInstalled
    @State private var binaryPath: String? = nil

    @State private var signInRunning: Bool = false
    @State private var pollAttempt: Int = 0
    @State private var testRunning: Bool = false
    @State private var testResult: DimmyCore.ClaudeCodePingResult? = nil
    @State private var copiedFlash: Bool = false

    private let npmCommand = "npm install -g @anthropic-ai/claude-code"

    var body: some View {
        VStack(spacing: 16) {
            header
            progressDots
            Divider()

            ScrollView {
                Group {
                    switch currentStep {
                    case .node: stepOne
                    case .claudeCli: stepTwo
                    case .signIn: stepThree
                    }
                }
                .padding(.vertical, 4)
            }
            .frame(minHeight: 280)

            Divider()
            footer
        }
        .padding(20)
        .frame(width: 560)
        .onAppear { probeAllAndSkip() }
    }

    // MARK: - Header / progress / footer

    private var header: some View {
        HStack(spacing: 10) {
            Image(systemName: "person.crop.circle.badge.checkmark")
                .font(.system(size: 18, weight: .semibold))
                .foregroundColor(Color(red: 0.84, green: 0.47, blue: 0.21))
            Text("Set up Claude subscription")
                .font(.system(size: 16, weight: .semibold))
            Spacer()
        }
    }

    private var progressDots: some View {
        HStack(spacing: 8) {
            ForEach([WizardStep.node, .claudeCli, .signIn], id: \.rawValue) { step in
                Circle()
                    .fill(step.rawValue <= currentStep.rawValue ? Color.accentColor : Color.gray.opacity(0.3))
                    .frame(width: 8, height: 8)
            }
        }
        .frame(maxWidth: .infinity)
    }

    private var footer: some View {
        HStack {
            Button("Cancel") { onClose() }
                .keyboardShortcut(.cancelAction)
            Spacer()
            if currentStep != .node {
                Button("Back") { goBack() }
            }
            Button(primaryButtonLabel) { handlePrimary() }
                .keyboardShortcut(.defaultAction)
                .disabled(!primaryButtonEnabled)
        }
    }

    private var primaryButtonLabel: String {
        switch currentStep {
        case .node, .claudeCli: return "Next"
        case .signIn:
            if let r = testResult, case .ok = r { return "Close & enable" }
            if claudeStatus == .ready { return "Test connection" }
            return "Next"
        }
    }

    private var primaryButtonEnabled: Bool {
        switch currentStep {
        case .node: return nodeStatus.found && nodeStatus.meetsMinimum
        case .claudeCli: return claudeStatus != .notInstalled
        case .signIn:
            if let r = testResult, case .ok = r { return true }
            return claudeStatus == .ready && !testRunning
        }
    }

    private func handlePrimary() {
        switch currentStep {
        case .node:
            currentStep = .claudeCli
            probeClaude()
        case .claudeCli:
            currentStep = .signIn
            probeClaude()
            if claudeStatus == .ready && testResult == nil { runTest() }
        case .signIn:
            if let r = testResult, case .ok = r {
                onComplete(true)
                onClose()
            } else if claudeStatus == .ready {
                runTest()
            }
        }
    }

    private func goBack() {
        switch currentStep {
        case .claudeCli: currentStep = .node; probeNode()
        case .signIn: currentStep = .claudeCli; probeClaude()
        default: break
        }
    }

    // MARK: - Step 1: Node.js

    private var stepOne: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("1 — Install Node.js")
                .font(.system(size: 18, weight: .semibold))
            Text("Claude Code is a Node.js CLI. We need Node 18 or newer before installing it. The official installer adds Node to your PATH automatically.")
                .font(.system(size: 13))
                .foregroundColor(.secondary)

            nodeStatusBadge

            if !(nodeStatus.found && nodeStatus.meetsMinimum) {
                HStack(spacing: 8) {
                    Button(action: openNodejs) {
                        Label("Open nodejs.org", systemImage: "arrow.up.right.square.fill")
                    }
                    .buttonStyle(.borderedProminent)
                    Button(action: recheckNode) {
                        Label("Recheck", systemImage: "arrow.clockwise")
                    }
                }
                Text("After installing, you may need to restart Dimmy so the new PATH is picked up.")
                    .font(.system(size: 12))
                    .foregroundColor(.secondary)
            }
        }
    }

    private var nodeStatusBadge: some View {
        HStack(spacing: 10) {
            if nodeStatus.found && nodeStatus.meetsMinimum {
                Image(systemName: "checkmark.circle.fill").foregroundColor(.green).font(.title2)
                Text(nodeStatus.version.map { "Node.js v\($0) detected" } ?? "Node.js detected")
            } else if nodeStatus.found {
                Image(systemName: "exclamationmark.triangle.fill").foregroundColor(.orange).font(.title2)
                Text(nodeStatus.version.map { "Node.js v\($0) found, but Claude needs v18 or newer." }
                     ?? "Node.js found but version too old (need v18+).")
            } else {
                Image(systemName: "xmark.circle.fill").foregroundColor(.red).font(.title2)
                Text("Node.js not found. Click below to install.")
            }
            Spacer()
        }
        .padding(12)
        .background(Color.gray.opacity(0.08))
        .cornerRadius(6)
    }

    private func openNodejs() {
        if let url = URL(string: "https://nodejs.org/en/download/") {
            NSWorkspace.shared.open(url)
        }
    }

    private func recheckNode() {
        DimmyCore.shared.recheckClaudeCode()
        nodeStatus = DimmyCore.shared.nodeStatus()
    }

    // MARK: - Step 2: Claude CLI

    private var stepTwo: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("2 — Install Claude CLI")
                .font(.system(size: 18, weight: .semibold))
            Text("The CLI ships via npm. Open a terminal, paste the command below, press return — npm downloads and installs claude-code globally.")
                .font(.system(size: 13))
                .foregroundColor(.secondary)

            claudeStatusBadge

            if claudeStatus == .notInstalled {
                VStack(alignment: .leading, spacing: 10) {
                    HStack {
                        Text(npmCommand)
                            .font(.system(.body, design: .monospaced))
                            .textSelection(.enabled)
                            .padding(10)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .background(Color.gray.opacity(0.12))
                            .cornerRadius(4)
                    }
                    HStack(spacing: 8) {
                        Button(action: copyNpmCommand) {
                            Label(copiedFlash ? "Copied!" : "Copy command",
                                  systemImage: "doc.on.doc")
                        }
                        .buttonStyle(.borderedProminent)
                        Button(action: openTerminal) {
                            Label("Open Terminal", systemImage: "terminal")
                        }
                        Button(action: recheckClaude) {
                            Label("Recheck", systemImage: "arrow.clockwise")
                        }
                    }
                    Text("If npm reports a permission error, run with a per-user prefix: `npm config set prefix ~/.npm-global` then add `~/.npm-global/bin` to your PATH.")
                        .font(.system(size: 12))
                        .foregroundColor(.secondary)
                }
            }
        }
    }

    private var claudeStatusBadge: some View {
        HStack(spacing: 10) {
            if claudeStatus != .notInstalled {
                Image(systemName: "checkmark.circle.fill").foregroundColor(.green).font(.title2)
                if let p = binaryPath {
                    Text("Claude CLI detected at \(p)")
                } else {
                    Text("Claude CLI detected")
                }
            } else {
                Image(systemName: "xmark.circle.fill").foregroundColor(.red).font(.title2)
                Text("Claude CLI not installed. Run the command below in a terminal.")
            }
            Spacer()
        }
        .padding(12)
        .background(Color.gray.opacity(0.08))
        .cornerRadius(6)
    }

    private func copyNpmCommand() {
        let pb = NSPasteboard.general
        pb.clearContents()
        pb.setString(npmCommand, forType: .string)
        copiedFlash = true
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) { copiedFlash = false }
    }

    private func openTerminal() {
        // Open a new Terminal window at the user's home dir. The
        // shell prompt is ready for paste. (We deliberately don't
        // auto-execute the npm command — the user controls when it
        // runs.)
        let script = "tell application \"Terminal\" to do script \"cd ~ && clear\""
        var error: NSDictionary? = nil
        NSAppleScript(source: script)?.executeAndReturnError(&error)
        NSWorkspace.shared.launchApplication("Terminal")
    }

    private func recheckClaude() {
        let s = DimmyCore.shared.recheckClaudeCode()
        claudeStatus = s
        binaryPath = DimmyCore.shared.claudeCodeBinaryPath
    }

    // MARK: - Step 3: Sign in + test

    private var stepThree: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("3 — Sign in to Claude")
                .font(.system(size: 18, weight: .semibold))
            Text("Click Sign in to open a Terminal window with the Claude CLI's browser-based OAuth flow. Complete it in your browser — Dimmy detects the login and runs a quick test.")
                .font(.system(size: 13))
                .foregroundColor(.secondary)

            VStack(alignment: .leading, spacing: 10) {
                signInBadge
                if testResult != nil || testRunning {
                    testBadge
                }
            }
            .padding(12)
            .background(Color.gray.opacity(0.08))
            .cornerRadius(6)

            HStack(spacing: 8) {
                if claudeStatus != .ready {
                    Button(action: signIn) {
                        Label("Sign in", systemImage: "person.badge.key.fill")
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(signInRunning)
                }
                Button(action: recheckSignIn) {
                    Label("Recheck", systemImage: "arrow.clockwise")
                }
                if let r = testResult, case .ok = r {} else if testResult != nil {
                    Button(action: { testResult = nil; runTest() }) {
                        Label("Retry test", systemImage: "arrow.clockwise")
                    }
                }
            }

            Text("Dimmy launches the Claude Code CLI as a local subprocess and uses its stored login. Requires your active Anthropic subscription. Subject to Anthropic's terms of service.")
                .font(.system(size: 11))
                .foregroundColor(.secondary)
                .padding(10)
                .background(Color.gray.opacity(0.06))
                .cornerRadius(6)
        }
    }

    private var signInBadge: some View {
        HStack(spacing: 10) {
            if claudeStatus == .ready {
                Image(systemName: "checkmark.circle.fill").foregroundColor(.green)
                Text("Signed in to Claude.")
            } else if claudeStatus == .notLoggedIn {
                if signInRunning {
                    ProgressView().controlSize(.small).scaleEffect(0.7)
                    Text("Complete the browser flow. Dimmy will detect when you're signed in…")
                } else {
                    Image(systemName: "exclamationmark.circle.fill").foregroundColor(.orange)
                    Text("Not signed in. Click Sign in to open the browser flow.")
                }
            } else {
                Image(systemName: "xmark.circle.fill").foregroundColor(.red)
                Text("Claude CLI missing — go back to Step 2.")
            }
            Spacer()
        }
        .font(.system(size: 13))
    }

    @ViewBuilder
    private var testBadge: some View {
        HStack(spacing: 10) {
            if testRunning {
                ProgressView().controlSize(.small).scaleEffect(0.7)
                Text("Test connection: pinging Claude…")
            } else if let r = testResult {
                switch r {
                case .ok(let ms):
                    Image(systemName: "checkmark.seal.fill").foregroundColor(.green)
                    Text("Test connection OK (\(ms) ms). Claude subscription ready.")
                default:
                    Image(systemName: "exclamationmark.octagon.fill").foregroundColor(.red)
                    Text("Test failed: \(describeTestResult(r))")
                }
            }
            Spacer()
        }
        .font(.system(size: 13))
    }

    private func describeTestResult(_ r: DimmyCore.ClaudeCodePingResult) -> String {
        switch r {
        case .ok: return ""
        case .notInstalled: return "Claude CLI not installed"
        case .notLoggedIn: return "Not signed in"
        case .spawnFailed: return "Couldn't spawn the CLI subprocess"
        case .timeout: return "Timed out (15 s)"
        case .nonZeroExit: return "CLI returned an error code"
        case .invalidUtf8: return "CLI returned invalid output"
        case .unknownError: return "Unknown error"
        }
    }

    private func signIn() {
        signInRunning = true
        DispatchQueue.global(qos: .userInitiated).async {
            let ok = DimmyCore.shared.spawnClaudeCodeLogin()
            DispatchQueue.main.async {
                if !ok {
                    signInRunning = false
                    return
                }
                pollAttempt = 0
                pollForCredentials()
            }
        }
    }

    private func pollForCredentials() {
        if pollAttempt >= 90 { // 3 minutes
            signInRunning = false
            return
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + 2.0) {
            let s = DimmyCore.shared.recheckClaudeCode()
            claudeStatus = s
            binaryPath = DimmyCore.shared.claudeCodeBinaryPath
            if s == .ready {
                signInRunning = false
                runTest()
                return
            }
            pollAttempt += 1
            pollForCredentials()
        }
    }

    private func recheckSignIn() {
        let s = DimmyCore.shared.recheckClaudeCode()
        claudeStatus = s
        binaryPath = DimmyCore.shared.claudeCodeBinaryPath
        if s == .ready && testResult == nil { runTest() }
    }

    private func runTest() {
        testRunning = true
        testResult = nil
        DispatchQueue.global(qos: .userInitiated).async {
            let r = DimmyCore.shared.pingClaudeCode()
            DispatchQueue.main.async {
                testRunning = false
                testResult = r
            }
        }
    }

    // MARK: - Probes / smart-skip

    private func probeAllAndSkip() {
        nodeStatus = DimmyCore.shared.nodeStatus()
        claudeStatus = DimmyCore.shared.claudeCodeStatus
        binaryPath = DimmyCore.shared.claudeCodeBinaryPath

        let nodeOk = nodeStatus.found && nodeStatus.meetsMinimum
        let claudeOk = claudeStatus != .notInstalled
        if !nodeOk {
            currentStep = .node
        } else if !claudeOk {
            currentStep = .claudeCli
        } else {
            currentStep = .signIn
            if claudeStatus == .ready { runTest() }
        }
    }

    private func probeNode() {
        nodeStatus = DimmyCore.shared.nodeStatus()
    }

    private func probeClaude() {
        claudeStatus = DimmyCore.shared.claudeCodeStatus
        binaryPath = DimmyCore.shared.claudeCodeBinaryPath
    }
}

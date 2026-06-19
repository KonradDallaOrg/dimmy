import SwiftUI
import AppKit

/// 2-step ChatGPT (Codex) subscription setup wizard, modal sheet.
/// Mac mirror of `Views/CodexConnectDialog.xaml` on Windows, and a
/// trimmed `ClaudeConnectSheet`. There is NO Node.js step because the
/// Codex CLI ships as a native standalone binary, installed via a
/// one-line shell command.
///
/// Flow:
///   Step 1 - Install the Codex CLI via `curl ... | sh` + detect the
///            `codex` binary.
///   Step 2 - Sign in (browser flow via `codex login`) + auto-fire test
///            ping.
///
/// Smart-skip: on appearance we probe the binary + login state once and
/// jump to the first incomplete step. A machine where the CLI is already
/// installed lands on Step 2 with test-ready state.
///
/// Completion contract: `onComplete` is called with `true` iff Step 2's
/// test ping returned positive. The caller (MacIntegrationsPage) then
/// flips the Output subscription routing so the integration is live.
///
/// Reuses the existing dimmy_codex_* FFI. No new native surface.
struct CodexConnectSheet: View {
    @ObservedObject var appState: AppState
    let onClose: () -> Void
    let onComplete: (Bool) -> Void

    private enum WizardStep: Int { case install = 1, signIn = 2 }

    @State private var currentStep: WizardStep = .install
    @State private var codexStatus: DimmyCore.ClaudeCodeStatus = .notInstalled
    @State private var binaryPath: String? = nil

    @State private var signInRunning: Bool = false
    @State private var pollAttempt: Int = 0
    @State private var testRunning: Bool = false
    @State private var testResult: DimmyCore.ClaudeCodePingResult? = nil
    @State private var copiedFlash: Bool = false

    private let installCommand = "curl -fsSL https://chatgpt.com/codex/install.sh | sh"

    var body: some View {
        VStack(spacing: 16) {
            header
            progressDots
            Divider()

            ScrollView {
                Group {
                    switch currentStep {
                    case .install: stepOne
                    case .signIn: stepTwo
                    }
                }
                .padding(.vertical, 4)
            }
            .frame(minHeight: 260)

            Divider()
            footer
        }
        .padding(20)
        .frame(width: 560)
        .onAppear { probeAndSkip() }
    }

    // MARK: - Header / progress / footer

    private var header: some View {
        HStack(spacing: 10) {
            // Real OpenAI logomark (Assets/Providers/openai.imageset),
            // template-rendered so it tints to the label colour.
            Image("openai")
                .resizable()
                .renderingMode(.template)
                .scaledToFit()
                .foregroundStyle(.primary)
                .frame(width: 20, height: 20)
            Text("Set up ChatGPT (Codex) subscription")
                .font(.system(size: 16, weight: .semibold))
            Spacer()
        }
    }

    private var progressDots: some View {
        HStack(spacing: 8) {
            ForEach([WizardStep.install, .signIn], id: \.rawValue) { step in
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
            if currentStep != .install {
                Button("Back") { goBack() }
            }
            Button(primaryButtonLabel) { handlePrimary() }
                .keyboardShortcut(.defaultAction)
                .disabled(!primaryButtonEnabled)
        }
    }

    private var primaryButtonLabel: String {
        switch currentStep {
        case .install: return "Next"
        case .signIn:
            if let r = testResult, case .ok = r { return "Close & enable" }
            if codexStatus == .ready { return "Test connection" }
            return "Next"
        }
    }

    private var primaryButtonEnabled: Bool {
        switch currentStep {
        case .install: return codexStatus != .notInstalled
        case .signIn:
            if let r = testResult, case .ok = r { return true }
            return codexStatus == .ready && !testRunning
        }
    }

    private func handlePrimary() {
        switch currentStep {
        case .install:
            currentStep = .signIn
            probeCodex()
            if codexStatus == .ready && testResult == nil { runTest() }
        case .signIn:
            if let r = testResult, case .ok = r {
                onComplete(true)
                onClose()
            } else if codexStatus == .ready {
                runTest()
            }
        }
    }

    private func goBack() {
        if currentStep == .signIn { currentStep = .install; probeCodex() }
    }

    // MARK: - Step 1: Install Codex CLI

    private var stepOne: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("1 - Install Codex")
                .font(.system(size: 18, weight: .semibold))
            Text("Open Terminal, paste this command, and press Enter. It installs the Codex CLI. No Node.js needed.")
                .font(.system(size: 13))
                .foregroundColor(.secondary)

            codexStatusBadge

            if codexStatus == .notInstalled {
                VStack(alignment: .leading, spacing: 10) {
                    HStack {
                        Text(installCommand)
                            .font(.system(.body, design: .monospaced))
                            .textSelection(.enabled)
                            .padding(10)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .background(Color.gray.opacity(0.12))
                            .cornerRadius(4)
                    }
                    HStack(spacing: 8) {
                        Button(action: copyInstallCommand) {
                            Label(copiedFlash ? "Copied!" : "Copy command",
                                  systemImage: "doc.on.doc")
                        }
                        .buttonStyle(.borderedProminent)
                        Button(action: openTerminal) {
                            Label("Open Terminal", systemImage: "terminal")
                        }
                        Button(action: recheckCodex) {
                            Label("Recheck", systemImage: "arrow.clockwise")
                        }
                    }
                    Text("After installing, you may need to restart Dimmy so the new PATH is picked up.")
                        .font(.system(size: 12))
                        .foregroundColor(.secondary)
                }
            }
        }
    }

    private var codexStatusBadge: some View {
        HStack(spacing: 10) {
            if codexStatus != .notInstalled {
                Image(systemName: "checkmark.circle.fill").foregroundColor(.green).font(.title2)
                if let p = binaryPath {
                    Text("Codex CLI detected at \(p)")
                } else {
                    Text("Codex CLI detected")
                }
            } else {
                Image(systemName: "xmark.circle.fill").foregroundColor(.red).font(.title2)
                Text("Codex CLI not installed. Run the command below in Terminal.")
            }
            Spacer()
        }
        .padding(12)
        .background(Color.gray.opacity(0.08))
        .cornerRadius(6)
    }

    private func copyInstallCommand() {
        let pb = NSPasteboard.general
        pb.clearContents()
        pb.setString(installCommand, forType: .string)
        copiedFlash = true
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) { copiedFlash = false }
    }

    private func openTerminal() {
        let script = "tell application \"Terminal\" to do script \"cd ~ && clear\""
        var error: NSDictionary? = nil
        NSAppleScript(source: script)?.executeAndReturnError(&error)
        NSWorkspace.shared.launchApplication("Terminal")
    }

    private func recheckCodex() {
        let s = DimmyCore.shared.recheckCodex()
        codexStatus = s
        binaryPath = DimmyCore.shared.codexBinaryPath
    }

    // MARK: - Step 2: Sign in + test

    private var stepTwo: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("2 - Sign in with ChatGPT")
                .font(.system(size: 18, weight: .semibold))
            Text("Click Sign in to open a Terminal window with the Codex sign-in. Log in with your ChatGPT Plus, Pro, Team, Business, or Enterprise plan. Dimmy detects the login and runs a quick test. A free ChatGPT account won't work for Codex.")
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
                if codexStatus != .ready {
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

            Text("Dimmy launches the Codex CLI as a local subprocess and uses its stored login. Requires an active paid ChatGPT plan. Dimmy never sees your token. Subject to OpenAI's terms of service.")
                .font(.system(size: 11))
                .foregroundColor(.secondary)
                .padding(10)
                .background(Color.gray.opacity(0.06))
                .cornerRadius(6)
        }
    }

    private var signInBadge: some View {
        HStack(spacing: 10) {
            if codexStatus == .ready {
                Image(systemName: "checkmark.circle.fill").foregroundColor(.green)
                Text("Signed in with your ChatGPT plan.")
            } else if codexStatus == .notLoggedIn {
                if signInRunning {
                    ProgressView().controlSize(.small).scaleEffect(0.7)
                    Text("Complete the ChatGPT sign-in. Dimmy will detect when you're signed in…")
                } else {
                    Image(systemName: "exclamationmark.circle.fill").foregroundColor(.orange)
                    Text("Not signed in. Click Sign in to open the ChatGPT login flow.")
                }
            } else {
                Image(systemName: "xmark.circle.fill").foregroundColor(.red)
                Text("Codex CLI missing. Go back to Step 1.")
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
                Text("Test connection: pinging ChatGPT…")
            } else if let r = testResult {
                switch r {
                case .ok(let ms):
                    Image(systemName: "checkmark.seal.fill").foregroundColor(.green)
                    Text("Test connection OK (\(ms) ms). ChatGPT subscription ready.")
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
        case .notInstalled: return "Codex CLI not installed"
        case .notLoggedIn: return "Not signed in"
        case .spawnFailed: return "Couldn't spawn the CLI subprocess"
        case .timeout: return "Timed out (30 s)"
        case .nonZeroExit: return "CLI returned an error code"
        case .invalidUtf8: return "CLI returned invalid output"
        case .unknownError: return "Unknown error"
        }
    }

    private func signIn() {
        signInRunning = true
        DispatchQueue.global(qos: .userInitiated).async {
            let ok = DimmyCore.shared.spawnCodexLogin()
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
            let s = DimmyCore.shared.recheckCodex()
            codexStatus = s
            binaryPath = DimmyCore.shared.codexBinaryPath
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
        let s = DimmyCore.shared.recheckCodex()
        codexStatus = s
        binaryPath = DimmyCore.shared.codexBinaryPath
        if s == .ready && testResult == nil { runTest() }
    }

    private func runTest() {
        testRunning = true
        testResult = nil
        DispatchQueue.global(qos: .userInitiated).async {
            let r = DimmyCore.shared.pingCodex()
            DispatchQueue.main.async {
                testRunning = false
                testResult = r
            }
        }
    }

    // MARK: - Probes / smart-skip

    private func probeAndSkip() {
        codexStatus = DimmyCore.shared.codexStatus
        binaryPath = DimmyCore.shared.codexBinaryPath
        if codexStatus == .notInstalled {
            currentStep = .install
        } else {
            currentStep = .signIn
            if codexStatus == .ready { runTest() }
        }
    }

    private func probeCodex() {
        codexStatus = DimmyCore.shared.codexStatus
        binaryPath = DimmyCore.shared.codexBinaryPath
    }
}

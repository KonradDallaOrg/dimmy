import SwiftUI
import AppKit

/// Guided 3-page Kimi Code setup, modal sheet. Mac mirror of
/// `Views/KimiConnectDialog.xaml` on Windows.
///
/// Flow:
///   Page 1 - Install : one button runs the official installer in Terminal.
///                      (No Git step: it is a Windows-only requirement.)
///   Page 2 - Sign in : `kimi login` (device code) in Terminal; Dimmy sees
///                      the sign-in by itself.
///   Page 3 - Test    : one ping through `kimi acp` + the plan's models.
///
/// Completion contract: `onComplete(true)` iff the test passed.
struct KimiConnectSheet: View {
    @ObservedObject var appState: AppState
    let onClose: () -> Void
    let onComplete: (Bool) -> Void

    /// Start at page 1 regardless of detection ("Re-run setup").
    var forceStartAtStep1: Bool = false

    private enum WizardStep: Int { case install = 1, signIn = 2, test = 3 }

    @State private var currentStep: WizardStep = .install
    @State private var status: DimmyCore.ClaudeCodeStatus = .notInstalled

    @State private var installStarted = false
    @State private var signInRunning = false
    @State private var signInTicks = 0
    @State private var pollTimer: Timer? = nil
    @State private var recheckInFlight = false

    @State private var testRunning = false
    @State private var testOk = false
    @State private var testMessage = ""
    @State private var modelsLine = ""

    private var kimiOk: Bool { status != .notInstalled }
    private var signedIn: Bool { status == .ready }

    var body: some View {
        VStack(spacing: 16) {
            header
            progressDots
            Divider()
            ScrollView {
                Group {
                    switch currentStep {
                    case .install: stepInstall
                    case .signIn: stepSignIn
                    case .test: stepTest
                    }
                }
                .padding(.vertical, 4)
            }
            .frame(minHeight: 240)
            Divider()
            footer
        }
        .padding(20)
        .frame(width: 560)
        .onAppear { probeAndSkip() }
        .onDisappear { stopPoll() }
    }

    // MARK: - Header / progress / footer

    private var header: some View {
        HStack(spacing: 10) {
            Image("kimi")
                .resizable()
                .scaledToFit()
                .frame(width: 20, height: 20)
            Text("Set up Kimi Code")
                .font(.system(size: 16, weight: .semibold))
            Spacer()
        }
    }

    private var progressDots: some View {
        HStack(spacing: 8) {
            ForEach([WizardStep.install, .signIn, .test], id: \.rawValue) { step in
                Circle()
                    .fill(step.rawValue <= currentStep.rawValue
                          ? Color.accentColor
                          : Color(nsColor: .systemGray))
                    .frame(width: 10, height: 10)
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
                Button("Back") { go(currentStep == .test ? .signIn : .install) }
            }
            switch currentStep {
            case .install:
                Button("Next") { go(signedIn ? .test : .signIn) }
                    .keyboardShortcut(.defaultAction)
                    .disabled(!kimiOk)
            case .signIn:
                Button("Next") { go(.test) }
                    .keyboardShortcut(.defaultAction)
                    .disabled(!signedIn)
            case .test:
                Button("Done") {
                    stopPoll()
                    onComplete(true)
                    onClose()
                }
                .keyboardShortcut(.defaultAction)
                .disabled(!testOk)
            }
        }
    }

    private func go(_ step: WizardStep) {
        stopPoll()
        currentStep = step
        switch step {
        case .install: if !kimiOk { startPoll() }
        case .signIn: if !signedIn { startPoll() }
        case .test: runTest()
        }
    }

    // MARK: - Page 1: Install

    private var stepInstall: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("1 - Install Kimi Code")
                .font(.system(size: 18, weight: .semibold))
            Text("Kimi Code is the official command-line app from Moonshot AI. The button opens Terminal and runs the official installer.")
                .font(.system(size: 13))
                .foregroundColor(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            HStack(spacing: 10) {
                if kimiOk {
                    Image(systemName: "checkmark.circle.fill").foregroundColor(.green).font(.title3)
                    Text("Kimi Code CLI installed.")
                } else if installStarted {
                    ProgressView().controlSize(.small).scaleEffect(0.7)
                    Text("Installing in Terminal. It appears here when done.")
                } else {
                    Image(systemName: "info.circle.fill").foregroundColor(.secondary)
                    Text("Kimi Code CLI not installed yet.")
                }
                Spacer()
            }
            .font(.system(size: 13))
            .padding(14)
            .background(Color.gray.opacity(0.08))
            .cornerRadius(6)

            if !kimiOk {
                Button("Install Kimi Code") { installNow() }
                    .buttonStyle(.borderedProminent)
            }
            if let url = URL(string: "https://www.kimi.com/code") {
                Link("See Kimi's official install page", destination: url)
                    .font(.system(size: 12))
            }
        }
    }

    private func installNow() {
        let cmd = DimmyCore.shared.kimiInstallCommand
        guard !cmd.isEmpty else { return }
        TerminalRunner.run(cmd, slug: "kimi-code")
        installStarted = true
        startPoll()
    }

    // MARK: - Page 2: Sign in

    private var stepSignIn: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("2 - Sign in")
                .font(.system(size: 18, weight: .semibold))
            Text("Terminal shows a code. Confirm it in the browser with your Kimi account. Kimi Code is included from the Plus plan. Dimmy never sees your token.")
                .font(.system(size: 13))
                .foregroundColor(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            HStack(spacing: 10) {
                if signedIn {
                    Image(systemName: "checkmark.circle.fill").foregroundColor(.green).font(.title3)
                    Text("Signed in to Kimi.")
                } else if signInRunning {
                    ProgressView().controlSize(.small).scaleEffect(0.7)
                    Text("Confirm the code in your browser. Dimmy detects the sign-in by itself.")
                } else {
                    Image(systemName: "info.circle.fill").foregroundColor(.secondary)
                    Text("Not signed in yet.")
                }
                Spacer()
            }
            .font(.system(size: 13))
            .padding(14)
            .background(Color.gray.opacity(0.08))
            .cornerRadius(6)

            if !signedIn {
                Button("Sign in with Kimi") { signIn() }
                    .buttonStyle(.borderedProminent)
                    .disabled(signInRunning)
            }
        }
    }

    private func signIn() {
        signInRunning = true
        DispatchQueue.global(qos: .userInitiated).async {
            let ok = DimmyCore.shared.spawnKimiLogin()
            DispatchQueue.main.async {
                if !ok {
                    signInRunning = false
                    DimmyCore.shared.trackEvent("kimi_cli.login_completed", ["outcome": "spawn_failed"])
                    return
                }
                signInTicks = 0
                startPoll()
            }
        }
    }

    // MARK: - Page 3: Test

    private var stepTest: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("3 - Test")
                .font(.system(size: 18, weight: .semibold))
            HStack(spacing: 10) {
                if testRunning {
                    ProgressView().controlSize(.small).scaleEffect(0.7)
                    Text("Sending a test message to Kimi...")
                } else {
                    Image(systemName: testOk ? "checkmark.circle.fill" : "exclamationmark.triangle.fill")
                        .foregroundColor(testOk ? .green : .orange)
                        .font(.title3)
                    Text(testMessage)
                }
                Spacer()
            }
            .font(.system(size: 13))
            .padding(14)
            .background(Color.gray.opacity(0.08))
            .cornerRadius(6)

            if !modelsLine.isEmpty {
                Text(modelsLine)
                    .font(.system(size: 12))
                    .foregroundColor(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Text("To use it, pick Kimi Code as the rewrite or recap model in Settings → Output and choose Subscription.")
                .font(.system(size: 12))
                .foregroundColor(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            Button("Test again") { runTest() }
                .disabled(testRunning)
        }
    }

    private func runTest() {
        testRunning = true
        testOk = false
        modelsLine = ""
        DispatchQueue.global(qos: .userInitiated).async {
            let result = DimmyCore.shared.pingKimi()
            var ok = false
            if case .ok = result { ok = true }
            let models = ok ? DimmyCore.shared.kimiModels() : []
            DispatchQueue.main.async {
                testRunning = false
                testOk = ok
                testMessage = Self.describe(result)
                if !models.isEmpty {
                    modelsLine = "Models in your plan: " + models.map(\.name).joined(separator: ", ")
                }
            }
        }
    }

    /// One message per ping result. Shared with `MacKimiCliCard`.
    static func describe(_ r: DimmyCore.KimiPingResult) -> String {
        switch r {
        case .ok(let ms): return "✓ Connection OK (\(ms) ms round-trip via the local `kimi` CLI)"
        case .notInstalled: return "✗ `kimi` not found. Install Kimi Code first."
        case .notSignedIn: return "✗ Not signed in. Sign in with your Kimi account."
        case .spawnFailed: return "✗ Could not start the Kimi CLI. See dimmy.log."
        case .timeout: return "✗ Timed out after 60 s, network or usage-limit issue."
        case .protocolError: return "✗ Unexpected answer from `kimi`. See dimmy.log."
        case .reported, .refused:
            return "✗ Kimi refused the request. The usual cause is the plan's usage limit; see dimmy.log."
        case .emptyResponse: return "✗ Kimi answered with nothing. Try again."
        case .unknownError: return "✗ Unknown error. See dimmy.log."
        }
    }

    // MARK: - Polling
    //
    // Documented exception to the no-FFI-polling rule: what is awaited is an
    // EXTERNAL change (an installer, a browser sign-in in another process).
    // The recheck runs off the main thread; one at a time. Stops on success,
    // after 3 minutes of sign-in, and on dismissal.

    private func startPoll() {
        stopPoll()
        let timer = Timer.scheduledTimer(withTimeInterval: 2.0, repeats: true) { _ in
            pollTick()
        }
        pollTimer = timer
        RunLoop.main.add(timer, forMode: .common)
    }

    private func pollTick() {
        guard !recheckInFlight else { return }
        recheckInFlight = true
        DispatchQueue.global(qos: .userInitiated).async {
            let s = DimmyCore.shared.recheckKimi()
            DispatchQueue.main.async {
                recheckInFlight = false
                let wasSignedIn = signedIn
                status = s
                switch currentStep {
                case .install:
                    if kimiOk { stopPoll() }
                case .signIn:
                    if signedIn {
                        if signInRunning && !wasSignedIn {
                            DimmyCore.shared.trackEvent("kimi_cli.login_completed", ["outcome": "success"])
                        }
                        signInRunning = false
                        stopPoll()
                    } else if signInRunning {
                        signInTicks += 1
                        if signInTicks >= 90 {
                            signInRunning = false
                            stopPoll()
                            DimmyCore.shared.trackEvent("kimi_cli.login_completed", ["outcome": "timeout"])
                        }
                    }
                case .test:
                    stopPoll()
                }
            }
        }
    }

    private func stopPoll() {
        pollTimer?.invalidate()
        pollTimer = nil
    }

    private func probeAndSkip() {
        status = DimmyCore.shared.kimiStatus
        if !forceStartAtStep1 && kimiOk {
            go(signedIn ? .test : .signIn)
        } else {
            go(.install)
        }
    }
}

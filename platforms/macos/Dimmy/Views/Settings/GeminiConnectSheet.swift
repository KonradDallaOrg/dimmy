import SwiftUI
import AppKit

/// Guided 3-page Gemini CLI setup wizard, modal sheet. Mac mirror of
/// `Views/GeminiConnectDialog.xaml` on Windows, and the same machine as
/// `CodexConnectSheet` so someone who has been through one recognises the
/// other.
///
/// Flow:
///   Page 1 - Install : pick an install command (npm / npx / brew).
///   Page 2 - Run     : open Terminal and auto-run it -> advance.
///   Page 3 - Finish  : poll for the binary, sign in if needed, green check.
///
/// Three things are specific to this CLI, and none of them is taste:
///
/// - **There is no `gemini login`.** A first run with no credentials shows
///   the auth picker itself, so Sign in launches the CLI bare and the user
///   chooses "Login with Google" in the terminal. Said on the page, not only
///   in the (i): someone expecting a browser to open by itself will sit and
///   wait for one.
/// - **It ships on npm**, so npm is the default tab and Node is a
///   prerequisite. There is no standalone installer as there is for Codex.
/// - **The npx tab installs nothing**, so its Finish page can never go
///   green. It is offered for trying the CLI out, and the page says so
///   rather than leaving the user watching a spinner forever.
///
/// And the thing that decides whether any of it works: Google ended Gemini
/// CLI access for personal accounts on 2026-06-18. Only Gemini Code Assist
/// Standard / Enterprise seats get past sign-in. The card that opens this
/// wizard is gated on the user declaring that; the wizard repeats it where
/// the refusal would actually appear.
struct GeminiConnectSheet: View {
    @ObservedObject var appState: AppState
    let onClose: () -> Void
    let onComplete: (Bool) -> Void

    /// Start at page 1 regardless of detection (the "Re-run wizard" entry
    /// point from the connected card).
    var forceStartAtStep1: Bool = false

    private enum WizardStep: Int { case install = 1, run = 2, finish = 3 }

    private enum CommandSource: String, CaseIterable, Identifiable {
        case npm, npx, brew
        var id: String { rawValue }
        var label: String {
            switch self {
            case .npm: return "npm"
            case .npx: return "npx"
            case .brew: return "brew"
            }
        }
        var command: String {
            switch self {
            case .npm: return "npm install -g @google/gemini-cli"
            case .npx: return "npx @google/gemini-cli"
            case .brew: return "brew install gemini-cli"
            }
        }
        var note: String {
            switch self {
            case .npm: return "Needs Node.js 20 or newer."
            case .npx:
                return "Runs it without installing. Good for a look, but Dimmy needs an installed `gemini` to call, so pick npm to finish setup."
            case .brew: return "Homebrew installs a managed Node alongside it."
            }
        }
    }

    @State private var currentStep: WizardStep = .install
    @State private var commandSource: CommandSource = .npm

    @State private var geminiStatus: DimmyCore.ClaudeCodeStatus = .notInstalled
    @State private var binaryPath: String? = nil

    @State private var signInRunning: Bool = false
    @State private var pollTimer: Timer? = nil

    @State private var showInstallInfo: Bool = false
    @State private var showRunInfo: Bool = false
    @State private var showFinishInfo: Bool = false

    private var geminiOk: Bool { geminiStatus != .notInstalled }
    private var signedIn: Bool { geminiStatus == .ready }

    var body: some View {
        VStack(spacing: 16) {
            header
            progressDots
            Divider()

            ScrollView {
                Group {
                    switch currentStep {
                    case .install: stepInstall
                    case .run: stepRun
                    case .finish: stepFinish
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
        .onDisappear { stopPoll() }
    }

    // MARK: - Header / progress / footer

    private var header: some View {
        HStack(spacing: 10) {
            // The real Gemini spark. NOT template-rendered: it carries its
            // own brand gradient and tinting would throw it away.
            Image("gemini")
                .resizable()
                .scaledToFit()
                .frame(width: 20, height: 20)
            Text("Set up Gemini")
                .font(.system(size: 16, weight: .semibold))
            Spacer()
        }
    }

    private var progressDots: some View {
        HStack(spacing: 8) {
            ForEach([WizardStep.install, .run, .finish], id: \.rawValue) { step in
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
                Button("Back") { goBack() }
            }
            Button("Done") { handleDone() }
                .keyboardShortcut(.defaultAction)
                .disabled(!signedIn)
        }
    }

    private func handleDone() {
        guard signedIn else { return }
        stopPoll()
        onComplete(true)
        onClose()
    }

    private func goBack() {
        stopPoll()
        switch currentStep {
        case .run: currentStep = .install
        case .finish: currentStep = .run
        case .install: break
        }
    }

    private func infoButton(isPresented: Binding<Bool>, text: String) -> some View {
        Button {
            isPresented.wrappedValue.toggle()
        } label: {
            Image(systemName: "info.circle")
                .font(.system(size: 14))
                .foregroundColor(.secondary)
        }
        .buttonStyle(.plain)
        .popover(isPresented: isPresented, arrowEdge: .bottom) {
            Text(text)
                .font(.system(size: 13))
                .frame(width: 320, alignment: .leading)
                .fixedSize(horizontal: false, vertical: true)
                .padding(12)
        }
    }

    // MARK: - Page 1: Install

    private var stepInstall: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 6) {
                Text("1 - Install the Gemini CLI")
                    .font(.system(size: 18, weight: .semibold))
                infoButton(
                    isPresented: $showInstallInfo,
                    text: "Google's official command-line tool, published on npm, so it needs Node.js 20 or newer. This is NOT the Gemini desktop app: that one has no way for another program to talk to it."
                )
            }

            Picker("", selection: $commandSource) {
                ForEach(CommandSource.allCases) { src in
                    Text(src.label).tag(src)
                }
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            .frame(maxWidth: 280, alignment: .leading)

            Text(commandSource.command)
                .font(.system(.body, design: .monospaced))
                .textSelection(.enabled)
                .padding(10)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(Color.gray.opacity(0.12))
                .cornerRadius(4)

            Text(commandSource.note)
                .font(.system(size: 12))
                .foregroundColor(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            if let url = URL(string: "https://github.com/google-gemini/gemini-cli") {
                Link("See Google's official install page", destination: url)
                    .font(.system(size: 12))
            }

            Button(action: copyCommandAndContinue) {
                Text("Copy command and continue")
            }
            .buttonStyle(.borderedProminent)
        }
    }

    private func copyCommandAndContinue() {
        let pb = NSPasteboard.general
        pb.clearContents()
        pb.setString(commandSource.command, forType: .string)
        currentStep = .run
    }

    // MARK: - Page 2: Run

    private var stepRun: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 6) {
                Text("2 - Run it")
                    .font(.system(size: 18, weight: .semibold))
                infoButton(
                    isPresented: $showRunInfo,
                    text: "Terminal opens and runs the install automatically. Wait until it finishes, then come back. The command is also on your clipboard as a fallback."
                )
            }

            Text("A terminal opens and runs the install for you. Wait for it to finish, then come back.")
                .font(.system(size: 13))
                .foregroundColor(.secondary)

            Button(action: installNow) {
                Text("Install now")
            }
            .buttonStyle(.borderedProminent)
        }
    }

    private func installNow() {
        TerminalRunner.run(commandSource.command, slug: "gemini-\(commandSource.rawValue)")
        currentStep = .finish
        startPoll()
    }

    // MARK: - Page 3: Finish

    private var stepFinish: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 6) {
                Text("3 - Finish")
                    .font(.system(size: 18, weight: .semibold))
                infoButton(
                    isPresented: $showFinishInfo,
                    text: "Dimmy runs the Gemini CLI as a local subprocess and uses the login it stored. Needs a Gemini Code Assist Standard or Enterprise licence: Google ended individual access, including AI Pro and Ultra, on 18 June 2026, so a personal account will fail here. Dimmy never reads your credentials."
                )
            }

            VStack(alignment: .leading, spacing: 10) {
                installStatusRow
                if geminiOk {
                    signInStatusRow
                }
            }
            .padding(14)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Color.gray.opacity(0.08))
            .cornerRadius(6)

            // On the page, not only behind the (i): the terminal shows an
            // auth menu, and someone waiting for a browser to open by itself
            // will wait a long time.
            if geminiOk && !signedIn {
                Text("The terminal asks how you want to authenticate. Choose 'Login with Google'. If it answers 'no longer supported for Gemini Code Assist for individuals', your account is a personal one and this route is closed.")
                    .font(.system(size: 12))
                    .foregroundColor(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }

            HStack(spacing: 8) {
                if geminiOk && !signedIn {
                    Button(action: signIn) {
                        Text("Sign in with Google")
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(signInRunning)
                }
                Button(action: recheck) {
                    Label("Recheck", systemImage: "arrow.clockwise")
                }
            }
        }
    }

    private var installStatusRow: some View {
        HStack(spacing: 10) {
            if geminiOk {
                Image(systemName: "checkmark.circle.fill")
                    .foregroundColor(.green)
                    .font(.title3)
                Text("Gemini CLI installed.")
            } else if commandSource == .npx {
                // npx installs nothing, so this row would spin forever.
                Image(systemName: "info.circle.fill")
                    .foregroundColor(.secondary)
                Text("npx doesn't install anything. Go Back and choose npm to finish setup.")
            } else {
                ProgressView().controlSize(.small).scaleEffect(0.7)
                Text("Run the command in your terminal. It appears here when done.")
            }
            Spacer()
        }
        .font(.system(size: 13))
    }

    private var signInStatusRow: some View {
        HStack(spacing: 10) {
            if signedIn {
                Image(systemName: "checkmark.circle.fill")
                    .foregroundColor(.green)
                    .font(.title3)
                Text("Signed in with Google.")
            } else if signInRunning {
                ProgressView().controlSize(.small).scaleEffect(0.7)
                Text("Complete the sign-in in Terminal. Dimmy detects it automatically.")
            } else {
                Image(systemName: "info.circle.fill")
                    .foregroundColor(.secondary)
                Text("Not signed in yet.")
            }
            Spacer()
        }
        .font(.system(size: 13))
    }

    private func signIn() {
        signInRunning = true
        DispatchQueue.global(qos: .userInitiated).async {
            let ok = DimmyCore.shared.spawnGeminiCliLogin()
            DispatchQueue.main.async {
                if !ok {
                    signInRunning = false
                    return
                }
                startPoll()
            }
        }
    }

    private func recheck() {
        geminiStatus = DimmyCore.shared.recheckGeminiCli()
        binaryPath = DimmyCore.shared.geminiCliBinaryPath
        if signedIn { signInRunning = false }
    }

    // MARK: - Polling
    //
    // Documented exception to the no-FFI-polling rule: what is being awaited
    // is an EXTERNAL change (npm finishing, a browser login in another
    // process). The core cannot emit an event for something it has not
    // observed. Stops on success and on sheet dismissal.

    private func startPoll() {
        stopPoll()
        let timer = Timer.scheduledTimer(withTimeInterval: 2.0, repeats: true) { _ in
            let prevSignedIn = signedIn
            geminiStatus = DimmyCore.shared.recheckGeminiCli()
            binaryPath = DimmyCore.shared.geminiCliBinaryPath
            if signedIn {
                signInRunning = false
                if !prevSignedIn { stopPoll() }
            }
        }
        pollTimer = timer
        RunLoop.main.add(timer, forMode: .common)
    }

    private func stopPoll() {
        pollTimer?.invalidate()
        pollTimer = nil
    }

    // MARK: - Probes / smart-skip

    private func probeAndSkip() {
        geminiStatus = DimmyCore.shared.geminiCliStatus
        binaryPath = DimmyCore.shared.geminiCliBinaryPath
        if !forceStartAtStep1 && geminiOk {
            currentStep = .finish
            startPoll()
        } else {
            currentStep = .install
        }
    }
}

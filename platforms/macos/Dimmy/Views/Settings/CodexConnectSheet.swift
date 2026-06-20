import SwiftUI
import AppKit

/// Guided 3-page ChatGPT (Codex) subscription setup wizard, modal sheet.
/// Mac mirror of `Views/CodexConnectDialog.xaml` on Windows. There is NO
/// Node.js step because the Codex CLI ships as a native standalone binary.
///
/// Flow:
///   Page 1 - Install : pick an install command (curl / brew / npm),
///                      Copy -> auto-advance.
///   Page 2 - Run     : open Terminal and auto-run the command -> advance.
///   Page 3 - Finish  : poll for the codex binary, sign in if needed,
///                      green check -> Done.
///
/// Minimal copy on each page; the extra detail lives behind the (i) info
/// buttons. One accent (blue) action per page: Copy -> Install now ->
/// Sign in / Done.
///
/// Smart-skip: on appearance we probe the binary + login state once. If
/// Codex is already Ready we open directly on page 3.
///
/// Completion contract: `onComplete` is called with `true` iff Codex is
/// Ready when the user presses Done. The caller (MacIntegrationsPage) then
/// flips the Output subscription routing so the integration is live.
///
/// Reuses the existing dimmy_codex_* FFI. No new native surface.
struct CodexConnectSheet: View {
    @ObservedObject var appState: AppState
    let onClose: () -> Void
    let onComplete: (Bool) -> Void

    /// Start at page 1 regardless of detection (the "Re-run setup" entry
    /// point from the connected card). Mirrors `ForceStartAtStep1`.
    var forceStartAtStep1: Bool = false

    private enum WizardStep: Int { case install = 1, run = 2, finish = 3 }

    private enum CommandSource: String, CaseIterable, Identifiable {
        case curl, brew, npm
        var id: String { rawValue }
        var label: String {
            switch self {
            case .curl: return "curl"
            case .brew: return "brew"
            case .npm: return "npm"
            }
        }
        var command: String {
            switch self {
            case .curl: return "curl -fsSL https://chatgpt.com/codex/install.sh | sh"
            case .brew: return "brew install --cask codex"
            case .npm: return "npm install -g @openai/codex"
            }
        }
    }

    @State private var currentStep: WizardStep = .install
    @State private var commandSource: CommandSource = .curl

    @State private var codexStatus: DimmyCore.ClaudeCodeStatus = .notInstalled
    @State private var binaryPath: String? = nil

    @State private var signInRunning: Bool = false
    @State private var pollTimer: Timer? = nil

    @State private var showInstallInfo: Bool = false
    @State private var showRunInfo: Bool = false
    @State private var showFinishInfo: Bool = false

    private var codexOk: Bool { codexStatus != .notInstalled }
    private var signedIn: Bool { codexStatus == .ready }

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

    /// Three step dots. Active dot = accent; inactive = a medium grey that
    /// reads clearly on both light and dark backgrounds (the Windows lesson:
    /// near-invisible dots on a light sheet are unacceptable).
    private var progressDots: some View {
        HStack(spacing: 8) {
            ForEach([WizardStep.install, .run, .finish], id: \.rawValue) { step in
                Circle()
                    .fill(step.rawValue <= currentStep.rawValue
                          ? Color.accentColor
                          : inactiveDotColor)
                    .frame(width: 10, height: 10)
            }
        }
        .frame(maxWidth: .infinity)
    }

    private var inactiveDotColor: Color {
        // Medium grey, opaque, visible on both themes. Color(.systemGray)
        // adapts to appearance and never washes out the way a low-opacity
        // grey does on a light sheet.
        Color(nsColor: .systemGray)
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
                .frame(width: 300, alignment: .leading)
                .fixedSize(horizontal: false, vertical: true)
                .padding(12)
        }
    }

    // MARK: - Page 1: Install

    private var stepInstall: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 6) {
                Text("1 - Install Codex")
                    .font(.system(size: 18, weight: .semibold))
                infoButton(
                    isPresented: $showInstallInfo,
                    text: "Codex is a small standalone CLI from OpenAI. No Node.js required. Homebrew and the install script both fetch a native binary."
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

            if let url = URL(string: "https://github.com/openai/codex") {
                Link("See OpenAI's official install page", destination: url)
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
                    text: "Terminal opens and runs the install automatically. Wait until it finishes, then come back. (The command is also on your clipboard as a fallback.)"
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
        runInTerminal(commandSource.command)
        currentStep = .finish
        startPoll()
    }

    /// Auto-run the install: write the command to a temp `.command` file
    /// and hand it to LaunchServices. Terminal.app opens .command files
    /// in a NEW window and auto-executes — no AppleScript, no TCC
    /// Automation prompt, and a fresh window every time so the user
    /// always sees the install start from scratch.
    private func runInTerminal(_ command: String) {
        TerminalRunner.run(command, slug: "codex-\(commandSource.rawValue)")
    }

    // MARK: - Page 3: Finish

    private var stepFinish: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 6) {
                Text("3 - Finish")
                    .font(.system(size: 18, weight: .semibold))
                infoButton(
                    isPresented: $showFinishInfo,
                    text: "Dimmy launches the Codex CLI as a local subprocess and uses its stored login. Requires a paid ChatGPT plan. Dimmy never sees your token. Subject to OpenAI's terms."
                )
            }

            VStack(alignment: .leading, spacing: 10) {
                installStatusRow
                if codexOk {
                    signInStatusRow
                }
            }
            .padding(14)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Color.gray.opacity(0.08))
            .cornerRadius(6)

            HStack(spacing: 8) {
                if codexOk && !signedIn {
                    Button(action: signIn) {
                        Text("Sign in with ChatGPT")
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
            if codexOk {
                Image(systemName: "checkmark.circle.fill")
                    .foregroundColor(.green)
                    .font(.title3)
                Text("Codex CLI installed.")
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
                Text("Signed in to ChatGPT.")
            } else if signInRunning {
                ProgressView().controlSize(.small).scaleEffect(0.7)
                Text("Complete the browser sign-in. Dimmy detects it automatically.")
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
            let ok = DimmyCore.shared.spawnCodexLogin()
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
        let s = DimmyCore.shared.recheckCodex()
        codexStatus = s
        binaryPath = DimmyCore.shared.codexBinaryPath
        if signedIn { signInRunning = false }
    }

    // MARK: - Polling

    private func startPoll() {
        stopPoll()
        let timer = Timer.scheduledTimer(withTimeInterval: 2.0, repeats: true) { _ in
            let prevSignedIn = signedIn
            let s = DimmyCore.shared.recheckCodex()
            codexStatus = s
            binaryPath = DimmyCore.shared.codexBinaryPath
            if signedIn {
                signInRunning = false
                if !prevSignedIn { stopPoll() } // fully done - stop hammering the CLI
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
        codexStatus = DimmyCore.shared.codexStatus
        binaryPath = DimmyCore.shared.codexBinaryPath
        if !forceStartAtStep1 && codexOk {
            currentStep = .finish
            startPoll()
        } else {
            currentStep = .install
        }
    }
}

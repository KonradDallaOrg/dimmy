import SwiftUI
import AppKit

/// Guided 3-page Claude Code subscription setup wizard, modal sheet.
/// Mac mirror of `Views/ClaudeConnectDialog.xaml` on Windows and the
/// sibling `CodexConnectSheet`. Linear, modal, re-runnable.
///
/// Claude Code ships as a native standalone binary now (no Node.js),
/// installed via a one-line shell command.
///
/// Flow:
///   Page 1 - Install : pick an install command (curl / npm), Copy ->
///                      auto-advance.
///   Page 2 - Run     : open Terminal and run the command for the user ->
///                      auto-advance.
///   Page 3 - Finish  : poll for the `claude` binary, sign in if needed,
///                      green check -> Done.
///
/// Minimal copy on each page; the extra detail lives behind the (i) info
/// popovers, like the rest of Settings. Default install command is curl:
/// it fetches a native binary with no Node.js required.
///
/// Smart-skip: on appearance we probe the status once and, unless the
/// caller forces step 1, jump straight to Finish when already Ready.
///
/// Completion contract: `onComplete` is called with `true` iff the wizard
/// reached the signed-in (Ready) state. The caller (MacIntegrationsPage)
/// then flips `llm_auth_method=subscription` so the integration is live
/// without a second click.
struct ClaudeConnectSheet: View {
    @ObservedObject var appState: AppState
    let onClose: () -> Void
    let onComplete: (Bool) -> Void

    /// Start at page 1 regardless of detection (the "Re-run setup" entry
    /// point from the connected card). Mirrors the Windows
    /// `ForceStartAtStep1`.
    var forceStartAtStep1: Bool = false

    private enum WizardPage: Int { case install = 1, run = 2, finish = 3 }

    private enum CommandSource: Int, CaseIterable, Identifiable {
        case curl, npm
        var id: Int { rawValue }
        var label: String { self == .curl ? "curl" : "npm" }
        var command: String {
            switch self {
            case .curl: return "curl -fsSL https://claude.ai/install.sh | bash"
            case .npm: return "npm install -g @anthropic-ai/claude-code"
            }
        }
    }

    @State private var currentPage: WizardPage = .install
    @State private var commandSource: CommandSource = .curl

    @State private var claudeStatus: DimmyCore.ClaudeCodeStatus = .notInstalled
    @State private var binaryPath: String? = nil

    @State private var signInRunning: Bool = false
    @State private var copiedFlash: Bool = false
    @State private var pollTimer: Timer? = nil

    private var selectedCommand: String { commandSource.command }
    private var claudeOk: Bool { claudeStatus != .notInstalled }
    private var ready: Bool { claudeStatus == .ready }

    var body: some View {
        VStack(spacing: 16) {
            header
            progressDots
            Divider()

            ScrollView {
                Group {
                    switch currentPage {
                    case .install: pageInstall
                    case .run: pageRun
                    case .finish: pageFinish
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
        .onAppear { probeAndSkip() }
        .onDisappear { stopPoll() }
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

    /// Three step dots, always visible. Active = accent; inactive = a
    /// fixed medium grey that reads on BOTH light and dark backgrounds
    /// (the key Windows lesson: a translucent/near-white inactive dot
    /// vanishes on a light sheet). We use an explicit opaque grey, not a
    /// low-opacity tint of the label colour.
    private var progressDots: some View {
        HStack(spacing: 8) {
            ForEach([WizardPage.install, .run, .finish], id: \.rawValue) { page in
                Circle()
                    .fill(page.rawValue <= currentPage.rawValue
                          ? Color.accentColor
                          : Color(red: 0.56, green: 0.56, blue: 0.56))
                    .frame(width: 8, height: 8)
            }
        }
        .frame(maxWidth: .infinity)
    }

    private var footer: some View {
        HStack {
            Button("Cancel") { stopPoll(); onClose() }
                .keyboardShortcut(.cancelAction)
            Spacer()
            if currentPage != .install {
                Button("Back") { goBack() }
            }
            if currentPage == .finish {
                Button("Done") {
                    stopPoll()
                    onComplete(true)
                    onClose()
                }
                .keyboardShortcut(.defaultAction)
                .buttonStyle(.borderedProminent)
                .disabled(!ready)
            }
        }
    }

    // MARK: - Page 1: Install

    private var pageInstall: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 6) {
                Text("1 - Install Claude Code")
                    .font(.system(size: 18, weight: .semibold))
                infoPopover(
                    "Claude Code is a small standalone CLI from Anthropic. No Node.js required. The install script fetches a native binary.")
            }

            Picker("", selection: $commandSource) {
                ForEach(CommandSource.allCases) { src in
                    Text(src.label).tag(src)
                }
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            .frame(maxWidth: 220, alignment: .leading)

            Text(selectedCommand)
                .font(.system(.body, design: .monospaced))
                .textSelection(.enabled)
                .padding(10)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(Color.gray.opacity(0.12))
                .cornerRadius(4)

            Link("See Anthropic's official install page",
                 destination: URL(string: "https://github.com/anthropics/claude-code")!)
                .font(.system(size: 12))

            Button(action: copyAndContinue) {
                Text(copiedFlash ? "Copied!" : "Copy command and continue")
            }
            .buttonStyle(.borderedProminent)
        }
    }

    private func copyAndContinue() {
        let pb = NSPasteboard.general
        pb.clearContents()
        pb.setString(selectedCommand, forType: .string)
        copiedFlash = true
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.2) { copiedFlash = false }
        enterPage(.run)
    }

    // MARK: - Page 2: Run

    private var pageRun: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 6) {
                Text("2 - Run it")
                    .font(.system(size: 18, weight: .semibold))
                infoPopover(
                    "Terminal opens and runs the install automatically. Wait until it finishes, then come back. (The command is also on your clipboard as a fallback.)")
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

    /// Auto-run: open Terminal.app and execute the selected install
    /// command for the user, so they do not have to paste or type. The
    /// command is also on the clipboard as a fallback (page 1 copied it).
    private func installNow() {
        let cmd = selectedCommand
        let escaped = cmd
            .replacingOccurrences(of: "\\", with: "\\\\")
            .replacingOccurrences(of: "\"", with: "\\\"")
        let script = """
        tell application "Terminal"
            do script "\(escaped)"
            activate
        end tell
        """
        var error: NSDictionary? = nil
        NSAppleScript(source: script)?.executeAndReturnError(&error)
        enterPage(.finish)
    }

    // MARK: - Page 3: Finish

    private var pageFinish: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 6) {
                Text("3 - Finish")
                    .font(.system(size: 18, weight: .semibold))
                infoPopover(
                    "Dimmy launches the Claude Code CLI as a local subprocess and uses its stored login. Requires your active Anthropic subscription. Dimmy never sees your token. Subject to Anthropic's terms.")
            }

            statusBox

            HStack(spacing: 8) {
                if claudeOk && !ready {
                    Button(action: signIn) {
                        Label("Sign in to Claude", systemImage: "person.badge.key.fill")
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

    private var statusBox: some View {
        VStack(alignment: .leading, spacing: 10) {
            // Install row.
            HStack(spacing: 10) {
                if !claudeOk {
                    ProgressView().controlSize(.small).scaleEffect(0.7)
                    Text("Run the command in your terminal. It appears here when done.")
                } else {
                    Image(systemName: "checkmark.circle.fill").foregroundColor(.green)
                    Text("Claude Code installed.")
                }
                Spacer()
            }
            .font(.system(size: 13))

            // Sign-in row (only once the binary is present).
            if claudeOk {
                HStack(spacing: 10) {
                    if ready {
                        Image(systemName: "checkmark.circle.fill").foregroundColor(.green)
                        Text("Signed in to Claude.")
                    } else if signInRunning {
                        ProgressView().controlSize(.small).scaleEffect(0.7)
                        Text("Complete the browser flow. Dimmy will detect when you're signed in.")
                    } else {
                        Image(systemName: "exclamationmark.circle.fill").foregroundColor(.orange)
                        Text("Not signed in yet.")
                    }
                    Spacer()
                }
                .font(.system(size: 13))
            }
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.gray.opacity(0.08))
        .cornerRadius(6)
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
                // Keep polling; the timer flips ready + stops itself.
                startPoll()
            }
        }
    }

    private func recheck() {
        let s = DimmyCore.shared.recheckClaudeCode()
        claudeStatus = s
        binaryPath = DimmyCore.shared.claudeCodeBinaryPath
        if ready { signInRunning = false; stopPoll() }
    }

    // MARK: - Polling (page 3 only)

    private func startPoll() {
        stopPoll()
        let t = Timer.scheduledTimer(withTimeInterval: 2.0, repeats: true) { _ in
            let s = DimmyCore.shared.recheckClaudeCode()
            claudeStatus = s
            binaryPath = DimmyCore.shared.claudeCodeBinaryPath
            if s == .ready {
                signInRunning = false
                stopPoll()
            }
        }
        RunLoop.main.add(t, forMode: .common)
        pollTimer = t
    }

    private func stopPoll() {
        pollTimer?.invalidate()
        pollTimer = nil
    }

    // MARK: - Navigation

    private func enterPage(_ page: WizardPage) {
        currentPage = page
        if page == .finish {
            probeClaude()
            if !ready { startPoll() } else { stopPoll() }
        } else {
            stopPoll()
        }
    }

    private func goBack() {
        switch currentPage {
        case .run: enterPage(.install)
        case .finish: enterPage(.run)
        case .install: break
        }
    }

    // MARK: - Probes / smart-skip

    private func probeAndSkip() {
        claudeStatus = DimmyCore.shared.claudeCodeStatus
        binaryPath = DimmyCore.shared.claudeCodeBinaryPath
        if !forceStartAtStep1 && ready {
            enterPage(.finish)
        } else {
            currentPage = .install
        }
    }

    private func probeClaude() {
        claudeStatus = DimmyCore.shared.claudeCodeStatus
        binaryPath = DimmyCore.shared.claudeCodeBinaryPath
    }

    // MARK: - Helpers

    private func infoPopover(_ text: String) -> some View {
        InfoPopoverButton(text: text)
    }
}

/// Small (i) button that shows an explanatory popover on click. Mirrors
/// the Windows info Flyout buttons next to each page heading.
private struct InfoPopoverButton: View {
    let text: String
    @State private var shown = false

    var body: some View {
        Button(action: { shown.toggle() }) {
            Image(systemName: "info.circle")
                .font(.system(size: 14))
                .foregroundColor(.secondary)
        }
        .buttonStyle(.plain)
        .popover(isPresented: $shown, arrowEdge: .bottom) {
            Text(text)
                .font(.system(size: 13))
                .frame(width: 300, alignment: .leading)
                .padding(12)
        }
    }
}

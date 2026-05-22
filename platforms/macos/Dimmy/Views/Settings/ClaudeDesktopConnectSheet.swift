import SwiftUI
import AppKit

/// 3-step Claude Desktop MCP connection wizard, modal sheet.
/// Mac mirror of `Views/ClaudeDesktopConnectDialog.xaml` on Windows.
///
/// Flow:
///   Step 1 — Detect Claude Desktop install (with Download link).
///   Step 2 — Confirm + patch claude_desktop_config.json.
///   Step 3 — Wait for first heartbeat (Claude Desktop must be restarted).
///
/// Completion contract: `onComplete` fires with `true` iff a fresh
/// heartbeat was observed (= bridge is alive). The caller then
/// updates the Settings card live.
struct ClaudeDesktopConnectSheet: View {
    @ObservedObject var appState: AppState
    let onClose: () -> Void
    let onComplete: (Bool) -> Void

    private enum WizardStep: Int { case detect = 1, patch = 2, verify = 3 }

    @State private var currentStep: WizardStep = .detect
    @State private var status: DimmyCore.ClaudeDesktopStatus = .empty
    @State private var patching: Bool = false
    @State private var patchError: String? = nil
    @State private var heartbeatTimer: Timer? = nil

    var body: some View {
        VStack(spacing: 16) {
            header
            progressDots
            Divider()

            ScrollView {
                Group {
                    switch currentStep {
                    case .detect: stepOne
                    case .patch: stepTwo
                    case .verify: stepThree
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
        .onDisappear { stopHeartbeatPoll() }
    }

    // MARK: - Header / progress / footer

    private var header: some View {
        HStack(spacing: 10) {
            Image(systemName: "link.circle.fill")
                .font(.system(size: 18, weight: .semibold))
                .foregroundColor(Color(red: 0.84, green: 0.47, blue: 0.21))
            Text("Connect Claude Desktop")
                .font(.system(size: 16, weight: .semibold))
            Spacer()
        }
    }

    private var progressDots: some View {
        HStack(spacing: 8) {
            ForEach(1...3, id: \.self) { i in
                Circle()
                    .fill(currentStep.rawValue >= i ? Color.accentColor : Color.gray.opacity(0.3))
                    .frame(width: 10, height: 10)
            }
        }
    }

    private var footer: some View {
        HStack(spacing: 8) {
            Spacer()
            if currentStep != .detect {
                Button("Back") {
                    if let prev = WizardStep(rawValue: currentStep.rawValue - 1) {
                        currentStep = prev
                        if prev != .verify { stopHeartbeatPoll() }
                    }
                }
            }
            Button("Cancel") {
                stopHeartbeatPoll()
                onClose()
            }
            primaryButton
        }
    }

    @ViewBuilder private var primaryButton: some View {
        switch currentStep {
        case .detect:
            Button("Next") {
                currentStep = .patch
            }
            .keyboardShortcut(.defaultAction)
            .disabled(!status.installed)
        case .patch:
            Button("Next") {
                currentStep = .verify
                startHeartbeatPoll()
            }
            .keyboardShortcut(.defaultAction)
            .disabled(!status.configPatched)
        case .verify:
            Button("Done") {
                onComplete(isAlive)
                stopHeartbeatPoll()
                onClose()
            }
            .keyboardShortcut(.defaultAction)
        }
    }

    // MARK: - Step 1 — detect

    private var stepOne: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("1 — Install Claude Desktop")
                .font(.system(size: 18, weight: .semibold))
            Text("Dimmy talks to Claude Desktop over MCP. You'll be able to ask Claude about your meetings, generate recaps from a different angle, and have Claude write notes back into Dimmy.")
                .font(.system(size: 13))
                .foregroundColor(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            VStack(alignment: .leading, spacing: 10) {
                HStack(spacing: 10) {
                    Image(systemName: status.installed ? "checkmark.circle.fill" : "exclamationmark.triangle.fill")
                        .font(.system(size: 18))
                        .foregroundColor(status.installed ? .green : .orange)
                    Text(status.installed
                         ? "Claude Desktop is installed."
                         : "We couldn't find Claude Desktop on this Mac.")
                        .font(.system(size: 13))
                    Spacer()
                }
                if let p = status.installPath {
                    Text(p).font(.system(size: 11, design: .monospaced))
                        .foregroundColor(.secondary)
                }
            }
            .padding(14)
            .background(RoundedRectangle(cornerRadius: 6).fill(Color(NSColor.controlBackgroundColor)))
            .overlay(RoundedRectangle(cornerRadius: 6).stroke(Color(NSColor.separatorColor)))

            if !status.installed {
                HStack(spacing: 10) {
                    Button("Download Claude Desktop") {
                        if let url = URL(string: "https://claude.ai/download") {
                            NSWorkspace.shared.open(url)
                        }
                    }
                    Button("I installed it, recheck") {
                        status = DimmyCore.shared.claudeDesktopStatus()
                    }
                }
            }
        }
    }

    // MARK: - Step 2 — patch

    private var stepTwo: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("2 — Register Dimmy with Claude Desktop")
                .font(.system(size: 18, weight: .semibold))
            Text("Dimmy will add itself to Claude Desktop's MCP servers list. Your existing servers stay untouched — we back up the file before changing it.")
                .font(.system(size: 13))
                .foregroundColor(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            VStack(alignment: .leading, spacing: 6) {
                Text("Extension folder").font(.system(size: 11)).foregroundColor(.secondary)
                Text(status.extensionPath ?? "(will be created on install)")
                    .font(.system(size: 12, design: .monospaced))
                Text("MCP binary").font(.system(size: 11)).foregroundColor(.secondary).padding(.top, 6)
                Text(resolveMcpBinaryPath() ?? "(not found in app bundle)")
                    .font(.system(size: 12, design: .monospaced))
            }
            .padding(14)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(RoundedRectangle(cornerRadius: 6).fill(Color(NSColor.controlBackgroundColor)))
            .overlay(RoundedRectangle(cornerRadius: 6).stroke(Color(NSColor.separatorColor)))

            HStack(spacing: 10) {
                Button(status.configPatched ? "Re-register" : "Connect") {
                    runPatch()
                }
                .disabled(patching)
                if patching {
                    ProgressView().controlSize(.small)
                }
                if status.configPatched && !patching {
                    Image(systemName: "checkmark.circle.fill").foregroundColor(.green)
                    Text("Registered.").font(.system(size: 13))
                }
                if let err = patchError {
                    Text(err).font(.system(size: 13)).foregroundColor(.red)
                }
            }
        }
    }

    // MARK: - Step 3 — verify heartbeat

    private var stepThree: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("3 — Restart Claude Desktop")
                .font(.system(size: 18, weight: .semibold))
            Text("Claude Desktop only spawns MCP servers at startup. Quit it (⌘Q) and reopen it. Dimmy will detect the first heartbeat automatically.")
                .font(.system(size: 13))
                .foregroundColor(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            HStack(spacing: 10) {
                if isAlive {
                    Image(systemName: "checkmark.circle.fill")
                        .font(.system(size: 18))
                        .foregroundColor(.green)
                    Text("Connected. Last heartbeat \(status.heartbeatAgeSecs ?? 0)s ago.")
                        .font(.system(size: 13))
                } else {
                    Image(systemName: "ellipsis.circle")
                        .font(.system(size: 18))
                        .foregroundColor(.secondary)
                    Text("Waiting for first heartbeat…").font(.system(size: 13))
                    ProgressView().controlSize(.small)
                }
                Spacer()
            }
            .padding(14)
            .background(RoundedRectangle(cornerRadius: 6).fill(Color(NSColor.controlBackgroundColor)))
            .overlay(RoundedRectangle(cornerRadius: 6).stroke(Color(NSColor.separatorColor)))

            Button("Open Claude Desktop") {
                if let path = status.installPath {
                    NSWorkspace.shared.open(URL(fileURLWithPath: path))
                }
            }
        }
    }

    // MARK: - Helpers

    private var isAlive: Bool {
        guard let age = status.heartbeatAgeSecs else { return false }
        return age < 90
    }

    private func probeAndSkip() {
        status = DimmyCore.shared.claudeDesktopStatus()
        // Smart-skip: jump to the first incomplete step.
        if !status.installed {
            currentStep = .detect
        } else if !status.configPatched {
            currentStep = .patch
        } else {
            currentStep = .verify
            startHeartbeatPoll()
        }
    }

    private func runPatch() {
        guard let binary = resolveMcpBinaryPath() else {
            patchError = "Couldn't locate dimmy-mcp in the app bundle."
            return
        }
        patching = true
        patchError = nil
        // Version stamp embedded in the manifest is cosmetic — the
        // Claude Connectors UI shows "Dimmy x.y.z" next to the icon.
        // Bundle CFBundleShortVersionString is the canonical Mac
        // source.
        let version = (Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String) ?? "0.0.0"
        DispatchQueue.global(qos: .userInitiated).async {
            let ok = DimmyCore.shared.installClaudeDesktopExtension(
                binaryPath: binary, version: version)
            DispatchQueue.main.async {
                patching = false
                if ok {
                    status = DimmyCore.shared.claudeDesktopStatus()
                } else {
                    patchError = "Install failed — see logs."
                }
            }
        }
    }

    private func startHeartbeatPoll() {
        stopHeartbeatPoll()
        // Documented polling exception (CLAUDE.md no-FFI-polling rule):
        // Claude Desktop's MCP-server-spawn has no notification API;
        // the only signal a server has started is the heartbeat file
        // appearing in our config dir.
        heartbeatTimer = Timer.scheduledTimer(withTimeInterval: 1.0, repeats: true) { _ in
            let fresh = DimmyCore.shared.claudeDesktopStatus()
            DispatchQueue.main.async {
                status = fresh
                if isAlive { stopHeartbeatPoll() }
            }
        }
    }

    private func stopHeartbeatPoll() {
        heartbeatTimer?.invalidate()
        heartbeatTimer = nil
    }

    /// Locate `dimmy-mcp` shipped inside the app bundle's Resources/.
    /// Bundle.main.bundlePath under .app gives us the canonical path
    /// even when launched from a stage dir during a Sparkle update.
    private func resolveMcpBinaryPath() -> String? {
        let resources = Bundle.main.bundlePath + "/Contents/Resources/dimmy-mcp"
        return FileManager.default.fileExists(atPath: resources) ? resources : nil
    }
}

import SwiftUI
import AppKit

/// 3-step Kimi Code Desktop MCP wizard, sheet. Mac mirror of
/// `Views/KimiDesktopConnectDialog.xaml` on Windows.
///
///   Step 1 - Install : detect Kimi Code Desktop; link to the official
///                      download page (macOS has no package manager entry).
///   Step 2 - Connect : register dimmy-mcp in ~/.kimi-code/mcp.json.
///   Step 3 - Verify  : open a NEW Kimi Code session; Dimmy sees dimmy-mcp's
///                      Kimi-only heartbeat.
struct KimiDesktopConnectSheet: View {
    let onClose: () -> Void
    var initialStep: Int = 1

    @State private var step = 1
    @State private var status = DimmyCore.KimiMcpStatus()
    @State private var connectMessage = ""
    @State private var pollTimer: Timer? = nil

    private var desktopOk: Bool { status.desktopPath != nil }
    private var heartbeatFresh: Bool { (status.heartbeatAgeSecs ?? Int.max) <= 90 }

    var body: some View {
        VStack(spacing: 16) {
            HStack(spacing: 10) {
                Image("kimi").resizable().scaledToFit().frame(width: 20, height: 20)
                Text("Connect Kimi Code Desktop").font(.system(size: 16, weight: .semibold))
                Spacer()
            }
            HStack(spacing: 8) {
                ForEach(1...3, id: \.self) { i in
                    Circle()
                        .fill(i <= step ? Color.accentColor : Color(nsColor: .systemGray))
                        .frame(width: 10, height: 10)
                }
            }
            .frame(maxWidth: .infinity)
            Divider()
            Group {
                switch step {
                case 1: stepInstall
                case 2: stepConnect
                default: stepVerify
                }
            }
            .frame(maxWidth: .infinity, minHeight: 220, alignment: .topLeading)
            Divider()
            HStack {
                Button("Cancel") { onClose() }.keyboardShortcut(.cancelAction)
                Spacer()
                if step > 1 { Button("Back") { go(step - 1) } }
                if step < 3 {
                    Button("Next") { go(step + 1) }
                        .keyboardShortcut(.defaultAction)
                        .disabled(step == 1 ? !desktopOk : !status.registered)
                } else {
                    Button("Done") { onClose() }.keyboardShortcut(.defaultAction)
                }
            }
        }
        .padding(20)
        .frame(width: 560)
        .onAppear { probe(); go(initialStep) }
        .onDisappear { stopPoll() }
    }

    private func row(_ ok: Bool, _ waiting: Bool, _ text: String) -> some View {
        HStack(spacing: 10) {
            if ok {
                Image(systemName: "checkmark.circle.fill").foregroundColor(.green).font(.title3)
            } else if waiting {
                ProgressView().controlSize(.small).scaleEffect(0.7)
            } else {
                Image(systemName: "info.circle.fill").foregroundColor(.secondary)
            }
            Text(text)
            Spacer()
        }
        .font(.system(size: 13))
        .padding(14)
        .background(Color.gray.opacity(0.08))
        .cornerRadius(6)
    }

    private var stepInstall: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("1 - Install Kimi Code Desktop").font(.system(size: 18, weight: .semibold))
            Text("Dimmy talks to Kimi Code Desktop over MCP. You can then ask Kimi about your meetings and dictations. It needs a Kimi plan that includes Kimi Code.")
                .font(.system(size: 13)).foregroundColor(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            row(desktopOk, false, desktopOk ? "Kimi Code Desktop installed." : "Kimi Code Desktop is not installed.")
            if !desktopOk, let url = URL(string: "https://www.kimi.ai/code") {
                Button("Download Kimi Code Desktop") { NSWorkspace.shared.open(url) }
                    .buttonStyle(.borderedProminent)
                Text("Install it, then come back: this page sees it by itself.")
                    .font(.system(size: 12)).foregroundColor(.secondary)
            }
        }
    }

    private var stepConnect: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("2 - Connect Dimmy").font(.system(size: 18, weight: .semibold))
            Text("Dimmy adds itself to Kimi Code's MCP servers. Your other servers stay as they are, and Dimmy keeps a copy of the original file.")
                .font(.system(size: 13)).foregroundColor(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            Text(status.configPath ?? "~/.kimi-code/mcp.json")
                .font(.system(.caption, design: .monospaced)).textSelection(.enabled)
            HStack(spacing: 10) {
                Button(status.registered ? "Connect again" : "Connect") { connect() }
                    .buttonStyle(.borderedProminent)
                if status.registered {
                    Image(systemName: "checkmark.circle.fill").foregroundColor(.green)
                }
                Text(connectMessage).font(.system(size: 13))
            }
        }
    }

    private var stepVerify: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("3 - Open a new session").font(.system(size: 18, weight: .semibold))
            Text("Kimi Code loads MCP servers when a session starts. Open Kimi Code Desktop and start a NEW session; sessions already open do not see Dimmy.")
                .font(.system(size: 13)).foregroundColor(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            row(heartbeatFresh, !heartbeatFresh,
                heartbeatFresh ? "Kimi Code started Dimmy. You can ask it about your meetings."
                               : "Waiting for Kimi Code to start Dimmy...")
            if let path = status.desktopPath {
                Button("Open Kimi Code Desktop") { NSWorkspace.shared.open(URL(fileURLWithPath: path)) }
            }
            Text("Then ask Kimi, for example: \"List my recent Dimmy meetings\".")
                .font(.system(size: 12)).foregroundColor(.secondary)
        }
    }

    private func connect() {
        let bin = Bundle.main.bundlePath + "/Contents/Resources/dimmy-mcp"
        let rc: Int32 = FileManager.default.fileExists(atPath: bin)
            ? DimmyCore.shared.installKimiMcp(binaryPath: bin) : -2
        switch rc {
        case 0: connectMessage = "Connected."
        case -2: connectMessage = "dimmy-mcp was not found in the app bundle."
        case -3: connectMessage = "~/.kimi-code/mcp.json is not valid JSON. Dimmy left it untouched."
        default: connectMessage = "Could not write ~/.kimi-code/mcp.json. See dimmy.log."
        }
        probe()
    }

    private func probe() { status = DimmyCore.shared.kimiMcpStatus }

    private func go(_ s: Int) {
        stopPoll()
        step = max(1, min(3, s))
        if (step == 1 && !desktopOk) || (step == 3 && !heartbeatFresh) { startPoll() }
    }

    // Documented exception to the no-FFI-polling rule: what is awaited
    // happens in another process (an install, Kimi Code starting dimmy-mcp).
    private func startPoll() {
        stopPoll()
        let t = Timer.scheduledTimer(withTimeInterval: 2.0, repeats: true) { _ in
            probe()
            if (step == 1 && desktopOk) || (step == 3 && heartbeatFresh) { stopPoll() }
        }
        pollTimer = t
        RunLoop.main.add(t, forMode: .common)
    }

    private func stopPoll() {
        pollTimer?.invalidate()
        pollTimer = nil
    }
}

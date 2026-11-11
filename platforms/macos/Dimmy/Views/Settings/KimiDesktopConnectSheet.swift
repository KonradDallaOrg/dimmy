import SwiftUI
import AppKit

/// 3-step Kimi (Kimi Work) wizard, sheet. Mac mirror of
/// `Views/KimiDesktopConnectDialog.xaml` on Windows.
///
///   Step 1 - Install : the Kimi app (download page on kimi.ai), opened once
///                      and signed in.
///   Step 2 - Connect : Dimmy adds its plugin to Kimi's Personal plugins (and
///                      ~/.kimi-code/mcp.json for Kimi Code); the user presses
///                      Install in Kimi.
///   Step 3 - Verify  : a Work chat starts dimmy-mcp (kimi-work heartbeat).
struct KimiDesktopConnectSheet: View {
    let onClose: () -> Void
    var initialStep: Int = 1

    @State private var step = 1
    @State private var status = DimmyCore.KimiMcpStatus()
    @State private var connectMessage = ""
    @State private var pollTimer: Timer? = nil

    private var appOk: Bool { status.workAppPath != nil }
    private var readyOk: Bool { appOk && status.workFound }
    private var heartbeatFresh: Bool { (status.workHeartbeatAgeSecs ?? Int.max) <= 90 }

    var body: some View {
        VStack(spacing: 16) {
            HStack(spacing: 10) {
                Image("kimi").resizable().scaledToFit().frame(width: 20, height: 20)
                Text("Connect Kimi").font(.system(size: 16, weight: .semibold))
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
                        .disabled(step == 1 ? !readyOk : !status.workInstalled)
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
        .padding(10)
        .background(Color.gray.opacity(0.08))
        .cornerRadius(6)
    }

    private var stepInstall: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("1 - Install Kimi").font(.system(size: 18, weight: .semibold))
            Text("Kimi Work, in the Kimi desktop app, can read your Dimmy meetings and dictations through a Dimmy plugin. Install Kimi, open it once and sign in.")
                .font(.system(size: 13)).foregroundColor(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            row(readyOk, false,
                readyOk ? "Kimi is installed and set up."
                    : appOk ? "Kimi is installed. Open it once and sign in." : "Kimi is not installed.")
            HStack(spacing: 10) {
                if !appOk, let url = URL(string: "https://www.kimi.ai/products/download") {
                    Button("Download Kimi") { NSWorkspace.shared.open(url) }
                        .buttonStyle(.borderedProminent)
                }
                if appOk && !readyOk { Button("Open Kimi") { openWork() } }
            }
        }
    }

    private var stepConnect: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("2 - Add the Dimmy plugin").font(.system(size: 18, weight: .semibold))
            Text("Dimmy adds its plugin to Kimi's Personal plugins. Then, in Kimi, open Plugins → Personal and press Install on Dimmy.")
                .font(.system(size: 13)).foregroundColor(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            row(status.workRegistered, false,
                status.workRegistered ? "Dimmy is in Kimi's Personal plugins." : "Not added yet.")
            row(status.workInstalled, status.workRegistered && !status.workInstalled,
                status.workInstalled ? "Installed in Kimi." : "In Kimi: Plugins → Personal → Dimmy → Install.")
            HStack(spacing: 10) {
                Button(status.workRegistered ? "Add again" : "Add to Kimi") { connect() }
                    .buttonStyle(.borderedProminent)
                Button("Open Kimi") { openWork() }
                Text(connectMessage).font(.system(size: 13))
            }
        }
    }

    private var stepVerify: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("3 - Try it").font(.system(size: 18, weight: .semibold))
            Text("In Kimi Work, start a new chat and ask: \"List my latest Dimmy meetings\". This page turns green when Kimi starts Dimmy.")
                .font(.system(size: 13)).foregroundColor(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            row(heartbeatFresh, !heartbeatFresh,
                heartbeatFresh ? "Kimi Work started Dimmy. Ask it about your meetings."
                               : "Waiting for Kimi to start Dimmy...")
            Button("Open Kimi Work") { openWork() }
        }
    }

    private func openWork() {
        if let url = URL(string: DimmyCore.kimiWorkLink) { NSWorkspace.shared.open(url) }
    }

    private func connect() {
        let bin = Bundle.main.bundlePath + "/Contents/Resources/dimmy-mcp"
        let rc: Int32 = FileManager.default.fileExists(atPath: bin)
            ? DimmyCore.shared.installKimiMcp(binaryPath: bin) : -2
        switch rc {
        case 0: connectMessage = ""; openWork()
        case -2: connectMessage = "dimmy-mcp was not found in the app bundle."
        case -3: connectMessage = "~/.kimi-code/mcp.json is not valid JSON. Dimmy left it untouched."
        default: connectMessage = "Could not write the plugin. See dimmy.log."
        }
        probe()
    }

    private func probe() { status = DimmyCore.shared.kimiMcpStatus }

    private func go(_ s: Int) {
        stopPoll()
        step = max(1, min(3, s))
        startPoll()
    }

    // Documented exception to the no-FFI-polling rule: what is awaited
    // happens in another process (an install, Install pressed in Kimi, Kimi
    // starting dimmy-mcp). 2 s while the sheet is open.
    private func startPoll() {
        let t = Timer.scheduledTimer(withTimeInterval: 2.0, repeats: true) { _ in probe() }
        pollTimer = t
        RunLoop.main.add(t, forMode: .common)
    }

    private func stopPoll() {
        pollTimer?.invalidate()
        pollTimer = nil
    }
}

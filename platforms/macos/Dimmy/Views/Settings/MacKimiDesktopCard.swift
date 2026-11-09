import SwiftUI

// MARK: - MacKimiDesktopCard
//
// Kimi Work card, mirror of the Windows "Kimi Work" card: status, Connect
// (opens KimiDesktopConnectSheet), Disconnect. Connect also registers Dimmy
// in ~/.kimi-code/mcp.json, so Kimi Code CLI and Kimi Code Desktop see it.

struct MacKimiDesktopCard: View {
    var onWizardRequested: (Int) -> Void

    @State private var status = DimmyCore.KimiMcpStatus()
    @State private var showDisconnect = false

    private var connected: Bool { status.workRegistered || status.registered }

    var body: some View {
        HStack(alignment: .top, spacing: 14) {
            Image("kimi").resizable().scaledToFit()
                .frame(width: 30, height: 30).frame(width: 40, height: 40)
            VStack(alignment: .leading, spacing: 6) {
                Text("Kimi Work").font(.system(size: 16, weight: .semibold))
                Text(statusText)
                    .font(.system(size: 13)).foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                HStack(spacing: 8) {
                    Button {
                        onWizardRequested(status.workInstalled ? 3
                            : status.workFound && status.workAppPath != nil ? 2 : 1)
                    } label: {
                        Label(connected ? "Re-run setup" : "Connect", systemImage: "wand.and.stars")
                    }
                    .controlSize(.small)
                    .buttonStyle(.borderedProminent)
                    if connected {
                        Button("Disconnect") { showDisconnect = true }.controlSize(.small)
                    }
                    Button(action: refresh) { Image(systemName: "arrow.clockwise") }
                        .controlSize(.small)
                }
                .padding(.top, 4)
            }
            Spacer()
            Image(systemName: status.workInstalled ? "checkmark.circle.fill" : "circle")
                .font(.system(size: 22))
                .foregroundStyle(status.workInstalled ? Color.green : Color.secondary)
        }
        .padding(16)
        .background(RoundedRectangle(cornerRadius: 8).fill(Color(NSColor.controlBackgroundColor)))
        .overlay(RoundedRectangle(cornerRadius: 8).stroke(Color.gray.opacity(0.25), lineWidth: 1))
        .onAppear { refresh() }
        .confirmationDialog("Disconnect Kimi?", isPresented: $showDisconnect, titleVisibility: .visible) {
            Button("Disconnect", role: .destructive) {
                DimmyCore.shared.uninstallKimiMcp()
                refresh()
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Dimmy removes its plugin from Kimi's Personal plugins and its entry from ~/.kimi-code/mcp.json. If Dimmy is installed in Kimi, remove it there too (Plugins → Installed).")
        }
    }

    func refresh() { status = DimmyCore.shared.kimiMcpStatus }

    private var statusText: String {
        if status.workInstalled, let age = status.workHeartbeatAgeSecs, age <= 120 {
            return "Connected. Kimi Work is using Dimmy right now."
        }
        if status.workInstalled { return "Connected. In Kimi Work, ask about your Dimmy meetings." }
        if status.workRegistered {
            return "Almost there: in Kimi, open Plugins → Personal and press Install on Dimmy."
        }
        return status.workAppPath != nil
            ? "Kimi is installed. Click Connect to let Kimi Work read your meetings."
            : "Let Kimi Work read your meetings and dictations. Click Connect to set it up."
    }
}

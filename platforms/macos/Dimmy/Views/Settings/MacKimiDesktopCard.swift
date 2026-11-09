import SwiftUI

// MARK: - MacKimiDesktopCard
//
// Kimi Code Desktop (MCP) card, mirror of the Windows "Kimi Code Desktop"
// card: status, Connect (opens KimiDesktopConnectSheet), Disconnect.

struct MacKimiDesktopCard: View {
    var onWizardRequested: (Int) -> Void

    @State private var status = DimmyCore.KimiMcpStatus()
    @State private var showDisconnect = false

    var body: some View {
        HStack(alignment: .top, spacing: 14) {
            Image("kimi").resizable().scaledToFit()
                .frame(width: 30, height: 30).frame(width: 40, height: 40)
            VStack(alignment: .leading, spacing: 6) {
                Text("Kimi Code Desktop").font(.system(size: 16, weight: .semibold))
                Text(statusText)
                    .font(.system(size: 13)).foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                HStack(spacing: 8) {
                    Button {
                        onWizardRequested(status.registered ? 3 : status.desktopPath != nil ? 2 : 1)
                    } label: {
                        Label(status.registered ? "Re-run setup" : "Connect", systemImage: "wand.and.stars")
                    }
                    .controlSize(.small)
                    .buttonStyle(.borderedProminent)
                    if status.registered {
                        Button("Disconnect") { showDisconnect = true }.controlSize(.small)
                    }
                    Button(action: refresh) { Image(systemName: "arrow.clockwise") }
                        .controlSize(.small)
                }
                .padding(.top, 4)
            }
            Spacer()
            Image(systemName: status.registered ? "checkmark.circle.fill" : "circle")
                .font(.system(size: 22))
                .foregroundStyle(status.registered ? Color.green : Color.secondary)
        }
        .padding(16)
        .background(RoundedRectangle(cornerRadius: 8).fill(Color(NSColor.controlBackgroundColor)))
        .overlay(RoundedRectangle(cornerRadius: 8).stroke(Color.gray.opacity(0.25), lineWidth: 1))
        .onAppear { refresh() }
        .confirmationDialog("Disconnect Kimi Code Desktop?", isPresented: $showDisconnect, titleVisibility: .visible) {
            Button("Disconnect", role: .destructive) {
                DimmyCore.shared.uninstallKimiMcp()
                refresh()
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Dimmy removes its entry from ~/.kimi-code/mcp.json. Your other servers stay in place.")
        }
    }

    func refresh() { status = DimmyCore.shared.kimiMcpStatus }

    private var statusText: String {
        let installed = status.desktopPath != nil
        if status.registered, let age = status.heartbeatAgeSecs, age <= 120 {
            return "Connected. Kimi Code is using Dimmy right now."
        }
        if status.registered {
            return installed
                ? "Connected. Start a new Kimi Code session to use Dimmy's tools."
                : "Registered for Kimi Code CLI. Kimi Code Desktop is not installed."
        }
        return installed
            ? "Kimi Code Desktop installed. Click Connect to let it read your meetings."
            : "Let Kimi Code Desktop read your meetings and dictations. Click Connect to set it up."
    }
}

import SwiftUI

// MARK: - MeetingIdleView
//
// Hero state shown when no meeting is in flight and no past meeting is
// selected. Mirror of the Win IdlePanel: large accent circle + glyph,
// "Ready to capture" headline, CTA subtitle, big Start button, "Generate
// recap on stop" toggle. Native Mac styling — no Win FluentIcon glyphs.

struct MeetingIdleView: View {
    @ObservedObject var vm: MeetingViewModel

    var body: some View {
        VStack(spacing: 22) {
            heroIcon
            VStack(spacing: 6) {
                Text("Ready to capture")
                    .font(.system(size: 22, weight: .semibold))
                Text("Long-form recording with structured recap. Pick an old meeting from the sidebar to review, or start a new one.")
                    .font(.system(size: 13))
                    .foregroundStyle(Color.macTextSecondary)
                    .multilineTextAlignment(.center)
                    .frame(maxWidth: 440)
            }
            Button(action: { vm.start() }) {
                HStack(spacing: 8) {
                    Image(systemName: "record.circle.fill")
                        .font(.system(size: 14))
                    Text("Start meeting")
                        .font(.system(size: 14, weight: .semibold))
                }
                .padding(.horizontal, 22)
                .padding(.vertical, 10)
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.large)
            .keyboardShortcut("r", modifiers: [.command])

            Toggle("Generate recap + action items on stop", isOn: $vm.generateRecap)
                .controlSize(.small)
                .toggleStyle(.checkbox)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private var heroIcon: some View {
        ZStack {
            Circle()
                .fill(Color.accentColor.opacity(0.18))
                .frame(width: 96, height: 96)
            Image(systemName: "mic.circle.fill")
                .font(.system(size: 48))
                .foregroundStyle(Color.accentColor)
        }
    }
}

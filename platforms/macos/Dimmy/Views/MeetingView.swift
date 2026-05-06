import SwiftUI
import AppKit

// MARK: - MeetingView
//
// Long-form recording UI. Mirror of Win MeetingWindow.xaml. Distinct
// from the dictation pill so the user can leave the window open, watch
// the live transcript drift in, and grab the recap + actions when they
// stop. State is held by `MeetingViewModel` (below); the Rust core owns
// the actual recording session and chunked transcribe loop.

struct MeetingView: View {
    @ObservedObject var vm: MeetingViewModel

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            header
            controls
            ScrollView {
                Text(vm.transcript.isEmpty
                     ? "🎙️ Listening… first transcript appears in ~15 s."
                     : vm.transcript)
                    .font(.system(size: 13))
                    .foregroundStyle(vm.transcript.isEmpty ? Color.secondary : Color.primary)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .textSelection(.enabled)
                    .padding(12)
            }
            .frame(minHeight: 200)
            .background(Color.gray.opacity(0.06))
            .cornerRadius(10)

            if vm.recapVisible {
                recapPanel
            }
            if vm.actionsVisible {
                actionsPanel
            }

            footer
        }
        .padding(16)
        .frame(minWidth: 640, minHeight: 480)
    }

    // MARK: Header

    private var header: some View {
        HStack(spacing: 10) {
            statusDot
            VStack(alignment: .leading, spacing: 2) {
                Text(vm.statusLabel)
                    .font(.system(size: 13, weight: .medium))
                Text(vm.subStatusLabel)
                    .font(.system(size: 11))
                    .foregroundStyle(.secondary)
            }
            Spacer()
            Text(vm.timerLabel)
                .font(.system(size: 22, weight: .semibold, design: .monospaced))
                .monospacedDigit()
        }
    }

    private var statusDot: some View {
        Circle()
            .fill(vm.dotColor)
            .frame(width: 10, height: 10)
            .shadow(color: vm.dotColor.opacity(0.6), radius: 4)
            .opacity(vm.isPulsing ? 0.5 : 1.0)
            .animation(
                vm.isRecording
                    ? .easeInOut(duration: 0.8).repeatForever(autoreverses: true)
                    : .default,
                value: vm.isPulsing
            )
            .onAppear { if vm.isRecording { vm.isPulsing = true } }
    }

    // MARK: Controls

    private var controls: some View {
        HStack(spacing: 10) {
            if !vm.isRecording {
                Button {
                    vm.start()
                } label: {
                    Label("Start meeting", systemImage: "record.circle.fill")
                        .foregroundStyle(.red)
                }
                .keyboardShortcut("r", modifiers: [.command])
                .disabled(vm.isWorking)
            } else {
                Button {
                    vm.stopAndProcess()
                } label: {
                    Label("Stop & process", systemImage: "stop.circle.fill")
                }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.return, modifiers: [.command])
                .disabled(vm.isWorking)
            }

            Toggle("Generate recap + actions", isOn: $vm.generateRecap)
                .controlSize(.small)
                .disabled(vm.isRecording || vm.isWorking)

            Spacer()

            if vm.isWorking {
                ProgressView().controlSize(.small).scaleEffect(0.7)
            }
        }
    }

    // MARK: Recap / Actions panels

    private var recapPanel: some View {
        VStack(alignment: .leading, spacing: 6) {
            Label("Recap", systemImage: "text.alignleft")
                .font(.system(size: 12, weight: .semibold))
                .foregroundStyle(.secondary)
            Text(vm.recap.isEmpty ? "(LLM produced no recap)" : vm.recap)
                .font(.system(size: 12))
                .frame(maxWidth: .infinity, alignment: .leading)
                .textSelection(.enabled)
                .padding(10)
                .background(Color.purple.opacity(0.08))
                .cornerRadius(8)
        }
    }

    private var actionsPanel: some View {
        VStack(alignment: .leading, spacing: 6) {
            Label("Action items", systemImage: "checklist")
                .font(.system(size: 12, weight: .semibold))
                .foregroundStyle(.secondary)
            Text(vm.actions.isEmpty ? "(no action items detected)" : vm.actions)
                .font(.system(size: 12))
                .frame(maxWidth: .infinity, alignment: .leading)
                .textSelection(.enabled)
                .padding(10)
                .background(Color.blue.opacity(0.08))
                .cornerRadius(8)
        }
    }

    // MARK: Footer

    private var footer: some View {
        HStack(spacing: 8) {
            if !vm.activeDir.isEmpty {
                Text(vm.activeDir)
                    .font(.system(size: 10, design: .monospaced))
                    .foregroundStyle(Color.secondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
                Button("Open folder") {
                    NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: vm.activeDir)])
                }
                .controlSize(.small)
            }
            Spacer()
            if let chunk = vm.chunkSummary {
                Text(chunk)
                    .font(.system(size: 11))
                    .foregroundStyle(.secondary)
            }
        }
    }
}

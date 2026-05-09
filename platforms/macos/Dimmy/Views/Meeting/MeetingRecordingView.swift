import SwiftUI

// MARK: - MeetingRecordingView
//
// Active-recording state. Mirror of Win RecordingPanel:
//   - persistent recording bar at the top with timer + chunks + Pause +
//     Stop buttons (always reachable regardless of which sub-panel is
//     showing below)
//   - app-context + live waveform card
//   - live transcript card
//
// The recording bar is Mac-native: SF Symbols, .borderedProminent
// Stop, system red as the destructive accent.

struct MeetingRecordingView: View {
    @ObservedObject var vm: MeetingViewModel

    var body: some View {
        VStack(spacing: 12) {
            recordingBar
            waveformCard
            transcriptCard
        }
    }

    // MARK: Recording bar

    private var recordingBar: some View {
        HStack(spacing: 12) {
            HStack(spacing: 8) {
                Circle()
                    .fill(vm.isPaused ? Color.orange : Color.red)
                    .frame(width: 10, height: 10)
                    .shadow(color: (vm.isPaused ? Color.orange : Color.red).opacity(0.6), radius: 4)
                Text(vm.isPaused ? "Paused" : "Recording")
                    .font(.system(size: 14, weight: .semibold))
                Text(vm.timerLabel)
                    .font(.system(size: 14, weight: .semibold, design: .monospaced))
                    .monospacedDigit()
                    .foregroundStyle(vm.isPaused ? Color.orange : Color.red)
                if !vm.chunkSummary.isEmpty {
                    Text(vm.chunkSummary)
                        .font(.system(size: 11))
                        .foregroundStyle(Color.macTextSecondary)
                }
            }
            Spacer()
            HStack(spacing: 6) {
                if vm.browsingPastMeeting {
                    Button(action: { vm.backToLive() }) {
                        HStack(spacing: 6) {
                            Image(systemName: "dot.radiowaves.left.and.right")
                                .font(.system(size: 12))
                            Text("Back to live")
                                .font(.system(size: 12))
                        }
                        .padding(.horizontal, 4)
                    }
                    .buttonStyle(.bordered)
                    .help("Return to the live recording view")
                }
                Button(action: { vm.togglePause() }) {
                    HStack(spacing: 6) {
                        Image(systemName: vm.isPaused ? "play.fill" : "pause.fill")
                            .font(.system(size: 11))
                        Text(vm.isPaused ? "Resume" : "Pause")
                            .font(.system(size: 12))
                    }
                    .padding(.horizontal, 4)
                }
                .buttonStyle(.bordered)
                .help("Pause / resume the meeting recording")

                Button(action: { vm.stopAndProcess() }) {
                    HStack(spacing: 6) {
                        Image(systemName: "stop.fill")
                            .font(.system(size: 11))
                        Text("Stop & finish")
                            .font(.system(size: 12, weight: .semibold))
                    }
                    .padding(.horizontal, 6)
                }
                .buttonStyle(.borderedProminent)
                .tint(.red)
                .keyboardShortcut(.return, modifiers: [.command])
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .background(panelBackground)
    }

    // MARK: Waveform card

    private var waveformCard: some View {
        HStack(spacing: 14) {
            HStack(spacing: 10) {
                Image(systemName: "mic.fill")
                    .font(.system(size: 14))
                    .foregroundStyle(Color.macTextSecondary)
                Text("Microphone")
                    .font(.system(size: 13))
            }
            // Live amplitude bars — VM updates 12× per second.
            HStack(alignment: .center, spacing: 3) {
                ForEach(Array(vm.liveAmplitudeBars.enumerated()), id: \.offset) { _, level in
                    RoundedRectangle(cornerRadius: 1.5, style: .continuous)
                        .fill(vm.isPaused ? Color.orange.opacity(0.6) : Color.accentColor)
                        .frame(width: 3, height: max(3, level * 36))
                }
            }
            .frame(maxWidth: .infinity)
            .frame(height: 44)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .background(panelBackground)
    }

    // MARK: Live transcript

    private var transcriptCard: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                Text("Live transcript")
                    .font(.system(size: 14, weight: .semibold))
                Spacer()
                if !vm.chunkSummary.isEmpty {
                    Text(vm.chunkSummary)
                        .font(.system(size: 11))
                        .foregroundStyle(Color.macTextTertiary)
                }
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 10)
            Divider().opacity(0.4)
            ScrollView {
                ScrollViewReader { proxy in
                    Text(vm.transcript.isEmpty
                         ? "🎙️ Listening… first chunk lands in ~15 s."
                         : vm.transcript)
                        .font(.system(size: 13))
                        .foregroundStyle(vm.transcript.isEmpty
                                          ? Color.macTextSecondary
                                          : Color.primary)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .textSelection(.enabled)
                        .padding(14)
                        .id("bottom")
                        .onChange(of: vm.transcript) { _, _ in
                            withAnimation(.easeOut(duration: 0.2)) {
                                proxy.scrollTo("bottom", anchor: .bottom)
                            }
                        }
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        .background(panelBackground)
    }

    private var panelBackground: some View {
        RoundedRectangle(cornerRadius: MacTheme.tileCornerRadius, style: .continuous)
            .fill(Color(nsColor: .windowBackgroundColor).opacity(0.6))
            .overlay(
                RoundedRectangle(cornerRadius: MacTheme.tileCornerRadius, style: .continuous)
                    .stroke(Color.macStrokeHairline, lineWidth: 0.5)
            )
    }
}

import SwiftUI
import AppKit

// MARK: - MeetingView
//
// Top-level meeting window content. Mirror of Win MeetingWindow.xaml:
//   - Title bar with "Dimmy Meeting" + center title + "New meeting"
//     button (always-visible escape from Done back to Idle).
//   - Body: 280pt sidebar | * main panel.
//   - Main panel hosts a state machine: idleView / recordingView /
//     processingView / doneView. The persistent recording bar inside
//     `MeetingRecordingView` keeps Stop/Pause reachable across
//     navigation, identical to Win.
//
// SourceKit may flag unresolved references to MeetingViewModel /
// MacTheme / Color extensions while indexing — those are defined in
// sibling files inside the same target and resolve at Xcode build time.

struct MeetingView: View {
    @StateObject private var vm: MeetingViewModel

    init(vm: MeetingViewModel) {
        _vm = StateObject(wrappedValue: vm)
    }

    var body: some View {
        VStack(spacing: 0) {
            titlebar
            Divider().opacity(0.4)
            if vm.phase == .recording && vm.browsingPastMeeting {
                liveRecordingBanner
            }
            if vm.systemAudioPermissionNeeded {
                systemAudioPermissionBanner
            }
            HStack(spacing: 0) {
                MeetingSidebar(vm: vm)
                mainPanel
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .padding(20)
            }
        }
        .overlay(alignment: .bottom) {
            if let toast = vm.toastMessage {
                Text(toast)
                    .font(.system(size: 13))
                    .foregroundStyle(.white)
                    .padding(.horizontal, 14)
                    .padding(.vertical, 8)
                    .background(
                        Capsule().fill(Color.black.opacity(0.78))
                    )
                    .padding(.bottom, 20)
                    .transition(.opacity.combined(with: .move(edge: .bottom)))
            }
        }
        .animation(.easeOut(duration: 0.18), value: vm.toastMessage)
        .animation(.easeInOut(duration: 0.22), value: vm.phase)
        .frame(minWidth: 880, minHeight: 560)
        .onAppear {
            vm.onWindowShown()
        }
    }

    // MARK: Titlebar

    private var titlebar: some View {
        HStack(spacing: 12) {
            HStack(spacing: 8) {
                Image(systemName: "mic.fill")
                    .font(.system(size: 14, weight: .semibold))
                    .foregroundStyle(Color.accentColor)
                Text("Dimmy Meeting")
                    .font(.system(size: 15, weight: .semibold))
            }
            Spacer()
            Text(vm.titlebarTitle)
                .font(.system(size: 13))
                .foregroundStyle(Color.macTextSecondary)
                .lineLimit(1)
                .truncationMode(.tail)
            Spacer()
            Button(action: { vm.newMeeting() }) {
                HStack(spacing: 6) {
                    Image(systemName: "plus")
                        .font(.system(size: 11, weight: .semibold))
                    Text("New meeting")
                        .font(.system(size: 13))
                }
                .padding(.horizontal, 4)
            }
            .buttonStyle(.bordered)
            .help("Start a new meeting")
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 12)
    }

    // MARK: Main panel

    @ViewBuilder
    private var mainPanel: some View {
        switch vm.phase {
        case .idle:
            MeetingIdleView(vm: vm)
        case .recording:
            // When the user clicked a past meeting in the sidebar while
            // the live recording is in flight, swap in MeetingDoneView
            // so they actually see the past meeting's content. The live
            // recording bar disappears from view, but the `liveRecordingBanner`
            // pinned above keeps "you're still recording" visible at a
            // glance plus a prominent Back-to-live CTA. The pollTimer
            // and FFI capture keep running untouched — nothing about
            // the actual recording changes here, only the on-screen
            // mapping.
            if vm.browsingPastMeeting {
                MeetingDoneView(vm: vm)
            } else {
                MeetingRecordingView(vm: vm)
            }
        case .processing:
            MeetingProcessingView(vm: vm)
        case .done:
            MeetingDoneView(vm: vm)
        }
    }

    // MARK: Live recording banner
    //
    // Pinned strip between the title bar and the sidebar/main split,
    // visible only when the user is browsing a past meeting while a
    // live recording is in flight. Carries the same red/orange dot +
    // timer pair used in the recording bar, plus a `.borderedProminent`
    // Back-to-live button (also bound to ⌘L). The banner spans the
    // full window width so the user can't miss it even with the
    // sidebar collapsed.
    /// Persistent banner shown while recording when the Core Audio tap
    /// delivered no frames — i.e. Dimmy lacks the system-audio-recording
    /// permission. Stays up (no auto-dismiss) with an Open-Settings CTA so
    /// the user can actually grant it, then restart the meeting. Replaces
    /// the old 2.5 s toast that vanished before the user could react.
    private var systemAudioPermissionBanner: some View {
        HStack(spacing: 10) {
            Image(systemName: "speaker.slash.fill")
                .font(.system(size: 13, weight: .semibold))
                .foregroundStyle(Color.orange)
            VStack(alignment: .leading, spacing: 2) {
                Text("System audio isn’t being captured")
                    .font(.system(size: 12, weight: .semibold))
                Text("Allow Dimmy to record this computer’s audio in System Settings → Privacy & Security, then restart the meeting. (Only mic is being recorded.)")
                    .font(.system(size: 11))
                    .foregroundStyle(Color.macTextSecondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer()
            Button(action: { vm.openSystemAudioSettings() }) {
                HStack(spacing: 6) {
                    Image(systemName: "gearshape.fill")
                        .font(.system(size: 12, weight: .semibold))
                    Text("Open System Settings")
                        .font(.system(size: 12, weight: .semibold))
                }
                .padding(.horizontal, 4)
            }
            .buttonStyle(.borderedProminent)
            .tint(.orange)
            .help("Open Privacy & Security so you can grant audio recording")
            Button(action: { vm.dismissSystemAudioBanner() }) {
                Image(systemName: "xmark")
                    .font(.system(size: 11, weight: .semibold))
            }
            .buttonStyle(.borderless)
            .help("Dismiss")
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 10)
        .background(
            Color.orange
                .opacity(0.08)
                .overlay(
                    Rectangle()
                        .fill(Color.orange.opacity(0.35))
                        .frame(height: 0.5),
                    alignment: .bottom
                )
        )
    }

    private var liveRecordingBanner: some View {
        HStack(spacing: 10) {
            Circle()
                .fill(vm.isPaused ? Color.orange : Color.red)
                .frame(width: 8, height: 8)
                .shadow(color: (vm.isPaused ? Color.orange : Color.red).opacity(0.6),
                        radius: 3)
            Text(vm.isPaused ? "Recording paused" : "Recording")
                .font(.system(size: 12, weight: .semibold))
            Text(vm.timerLabel)
                .font(.system(size: 12, weight: .medium, design: .monospaced))
                .monospacedDigit()
                .foregroundStyle(vm.isPaused ? Color.orange : Color.red)
            Text("· viewing past meeting")
                .font(.system(size: 11))
                .foregroundStyle(Color.macTextSecondary)
            Spacer()
            Button(action: { vm.backToLive() }) {
                HStack(spacing: 6) {
                    Image(systemName: "dot.radiowaves.left.and.right")
                        .font(.system(size: 12, weight: .semibold))
                    Text("Back to live")
                        .font(.system(size: 12, weight: .semibold))
                }
                .padding(.horizontal, 4)
            }
            .buttonStyle(.borderedProminent)
            .tint(.accentColor)
            .keyboardShortcut("l", modifiers: [.command])
            .help("Return to the live recording view (⌘L)")
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 8)
        .background(
            (vm.isPaused ? Color.orange : Color.red)
                .opacity(0.08)
                .overlay(
                    Rectangle()
                        .fill((vm.isPaused ? Color.orange : Color.red).opacity(0.35))
                        .frame(height: 0.5),
                    alignment: .bottom
                )
        )
    }
}

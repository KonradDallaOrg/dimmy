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
            MeetingRecordingView(vm: vm)
        case .processing:
            MeetingProcessingView(vm: vm)
        case .done:
            MeetingDoneView(vm: vm)
        }
    }
}

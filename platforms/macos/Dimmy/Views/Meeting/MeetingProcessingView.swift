import SwiftUI

// MARK: - MeetingProcessingView
//
// Transient state shown between Stop and Done while the recap pipeline
// runs. Mirror of Win ProcessingPanel: progress ring + step list.
// `vm.processingStep` advances as each phase completes (saving →
// generating recap → extracting actions).

struct MeetingProcessingView: View {
    @ObservedObject var vm: MeetingViewModel

    var body: some View {
        VStack(spacing: 22) {
            // During a meeting re-transcription the core emits per-chunk
            // progress, so show a determinate bar + percent; the recap step
            // (and the post-stop pipeline) have no progress events and fall
            // back to the indeterminate spinner.
            if let pct = vm.retranscribePercent {
                ProgressView(value: pct, total: 100)
                    .frame(width: 260)
                Text("Transcribing audio… \(Int(pct))%")
                    .font(.system(size: 20, weight: .semibold))
            } else {
                ProgressView()
                    .controlSize(.large)
                    .scaleEffect(1.4)
                Text("Wrapping up...")
                    .font(.system(size: 20, weight: .semibold))
            }
            VStack(alignment: .leading, spacing: 10) {
                step(title: "Saved audio + transcripts",
                     icon: stepIconName(.saving),
                     color: stepIconColor(.saving))
                step(title: "Generating recap with LLM...",
                     icon: stepIconName(.generatingRecap),
                     color: stepIconColor(.generatingRecap))
                step(title: "Extracting action items",
                     icon: stepIconName(.extractingActions),
                     color: stepIconColor(.extractingActions))
            }
            .frame(minWidth: 320, alignment: .leading)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private func step(title: String, icon: String, color: Color) -> some View {
        HStack(spacing: 10) {
            Image(systemName: icon)
                .font(.system(size: 14, weight: .semibold))
                .foregroundStyle(color)
                .frame(width: 18)
            Text(title)
                .font(.system(size: 13))
        }
    }

    private func stepIconName(_ step: MeetingViewModel.ProcessingStep) -> String {
        if reachedOrPassed(step) {
            return reachedAndDone(step) ? "checkmark.circle.fill" : "circle.dashed"
        }
        return "circle"
    }

    private func stepIconColor(_ step: MeetingViewModel.ProcessingStep) -> Color {
        if reachedAndDone(step) { return .green }
        if reachedOrPassed(step) { return .accentColor }
        return Color.macTextTertiary
    }

    private func reachedOrPassed(_ step: MeetingViewModel.ProcessingStep) -> Bool {
        order(vm.processingStep) >= order(step)
    }

    private func reachedAndDone(_ step: MeetingViewModel.ProcessingStep) -> Bool {
        order(vm.processingStep) > order(step)
    }

    private func order(_ step: MeetingViewModel.ProcessingStep) -> Int {
        switch step {
        case .saving: return 0
        case .generatingRecap: return 1
        case .extractingActions: return 2
        }
    }
}

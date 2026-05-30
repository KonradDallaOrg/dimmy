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
    /// Focus tracker for the Notes editor — used to save on blur, same
    /// pattern as MeetingDoneView. SwiftUI's TextEditor has no native
    /// "lost focus" callback; the @FocusState onChange handler is it.
    @FocusState private var notesFocused: Bool

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
                // Back-to-live CTA lives in the parent MeetingView's
                // `liveRecordingBanner` now — it's only relevant when
                // `vm.browsingPastMeeting == true`, and that state
                // routes the main panel to MeetingDoneView (not here),
                // so this button would have been unreachable from this
                // view.
                Button(action: { vm.togglePause() }) {
                    HStack(spacing: 6) {
                        Image(systemName: vm.isPaused ? "play.fill" : "pause.fill")
                            .font(.system(size: 13, weight: .medium))
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
                            .font(.system(size: 13, weight: .semibold))
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
            VStack(alignment: .leading, spacing: 4) {
                HStack(spacing: 6) {
                    Image(systemName: "mic.fill")
                        .font(.system(size: 12, weight: .medium))
                        .foregroundStyle(DualBandWaveform.micColor)
                    Text("Mic")
                        .font(.system(size: 12, weight: .medium))
                }
                if vm.systemAudioActive {
                    HStack(spacing: 6) {
                        Image(systemName: "speaker.wave.2.fill")
                            .font(.system(size: 12, weight: .medium))
                            .foregroundStyle(DualBandWaveform.systemColor)
                        Text("System")
                            .font(.system(size: 12, weight: .medium))
                    }
                }
            }
            .foregroundStyle(Color.macTextSecondary)
            .frame(width: 64, alignment: .leading)

            // Live amplitude bars — VM updates 12× per second from the
            // FFI peak (mic + system). Bars grow up from a centre line:
            // mic above, system audio below (mirrored). When system is
            // 0 across the buffer, the lower band collapses and we
            // effectively show a single-band mic waveform.
            DualBandWaveform(
                samples: vm.liveAmplitudeBars,
                paused: vm.isPaused
            )
            .frame(maxWidth: .infinity)
            .frame(height: 44)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .background(panelBackground)
    }

    // MARK: Live transcript / Notes (tabbed)

    private var transcriptCard: some View {
        VStack(alignment: .leading, spacing: 0) {
            recordingTabHeader
            Divider().opacity(0.4)
            // Mutually exclusive content. The notes editor shares the
            // SAME `vm.doneNotes` buffer + `<dir>/notes.md` on disk as
            // the Done-view Notes tab — single store, so a note typed
            // here survives into Done without an extra load step.
            if vm.recordingSelectedTab == .live {
                liveTranscriptScroll
            } else {
                notesEditor
            }
        }
        .background(panelBackground)
        // Save when the user leaves the Notes tab — mirrors the Done-
        // view onChange save (MeetingDoneView line ~169). Stop +
        // newMeeting already call saveNotes too, so a tab-leave save
        // is the only NEW persistence trigger this view introduces.
        .onChange(of: vm.recordingSelectedTab) { oldTab, _ in
            if oldTab == .notes { vm.saveNotes() }
        }
    }

    private var recordingTabHeader: some View {
        HStack {
            Picker("", selection: $vm.recordingSelectedTab) {
                ForEach(MeetingViewModel.RecordingTab.allCases) { tab in
                    Text(tab.title).tag(tab)
                }
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            .frame(maxWidth: 280)
            Spacer()
            if vm.recordingSelectedTab == .notes {
                Button(action: {
                    vm.stampMeetingTime()
                    notesFocused = true
                }) {
                    HStack(spacing: 4) {
                        Image(systemName: "plus.circle")
                        Text("Stamp time")
                    }
                    .font(.system(size: 12))
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .help("Insert a [mm:ss] stamp at the end of your notes")
            } else if !vm.chunkSummary.isEmpty {
                Text(vm.chunkSummary)
                    .font(.system(size: 11))
                    .foregroundStyle(Color.macTextTertiary)
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
    }

    private var liveTranscriptScroll: some View {
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

    private var notesEditor: some View {
        ZStack(alignment: .topLeading) {
            if vm.doneNotes.isEmpty {
                Text("Your notes — stamp the current time with the button, then type. Saved to notes.md, also visible from the Done view.")
                    .font(.system(size: 13))
                    .foregroundStyle(Color.macTextSecondary.opacity(0.7))
                    .padding(.horizontal, 18)
                    .padding(.vertical, 16)
                    .allowsHitTesting(false)
            }
            TextEditor(text: $vm.doneNotes)
                .font(.system(size: 13))
                .scrollContentBackground(.hidden)
                .padding(.horizontal, 14)
                .padding(.vertical, 12)
                .focused($notesFocused)
                .onChange(of: notesFocused) { _, isFocused in
                    if !isFocused { vm.saveNotes() }
                }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
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

// MARK: - DualBandWaveform
//
// Mirrored two-band waveform: mic level grows up from the centre line,
// system-audio level grows down. Width-aware: the bar count tracks the
// available pixels so resizing the window keeps a fixed bar+gap pixel
// width (matches the Win behaviour in
// `MeetingWindow.LiveWaveformCanvas_SizeChanged`).
//
// The view reads from `MeetingViewModel.liveAmplitudeBars` which is a
// scrolling FIFO of the last N FFI samples — so the bars truly
// represent recent audio history, not a single instantaneous value
// repeated N times.

struct DualBandWaveform: View {
    let samples: [MeetingAmplitudeSample]
    let paused: Bool

    private static let barWidth: CGFloat = 3
    private static let barGap: CGFloat = 2
    private static let cornerRadius: CGFloat = 1.5

    // Match the playback waveform colours in
    // `AudioPlaybackBar.DualBandWaveformStrip` so the live view and the
    // Done-tab playback view are visually identical: DodgerBlue for the
    // mic band (above the centre line), LimeGreen for the system-audio
    // band (below). Before this they both used `Color.accentColor` with
    // a 0.55 opacity on the system band, which read as a single
    // mirrored mic waveform.
    static let micColor = Color(red: 0.118, green: 0.565, blue: 1.000)
    static let systemColor = Color(red: 0.196, green: 0.804, blue: 0.196)

    var body: some View {
        GeometryReader { proxy in
            let centreY = proxy.size.height / 2
            let halfHeight = max(2, centreY - 1)
            let visible = visibleSamples(for: proxy.size.width)
            HStack(alignment: .center, spacing: Self.barGap) {
                ForEach(Array(visible.enumerated()), id: \.offset) { _, sample in
                    barView(sample: sample, halfHeight: halfHeight)
                }
            }
            .frame(width: proxy.size.width, height: proxy.size.height, alignment: .trailing)
            .opacity(paused ? 0.45 : 1.0)
            .animation(.easeOut(duration: 0.08), value: samples)
            .accessibilityHidden(true)
        }
    }

    private func barView(sample: MeetingAmplitudeSample,
                         halfHeight: CGFloat) -> some View {
        // Two stacked rounded rects, mirrored: mic on top, system on
        // bottom. Min-height of 2 keeps the bar visible even when the
        // signal is silent so the waveform stays anchored.
        VStack(spacing: 0) {
            RoundedRectangle(cornerRadius: Self.cornerRadius, style: .continuous)
                .fill(Self.micColor)
                .frame(width: Self.barWidth,
                       height: max(2, sample.mic * halfHeight))
            RoundedRectangle(cornerRadius: Self.cornerRadius, style: .continuous)
                .fill(Self.systemColor)
                .frame(width: Self.barWidth,
                       height: max(2, sample.system * halfHeight))
        }
        .frame(width: Self.barWidth, height: halfHeight * 2, alignment: .center)
    }

    /// Pick the trailing N samples that fit at fixed bar+gap pixel size.
    /// The FIFO is already capacity-bound but the window may be wider or
    /// narrower; trim to width so resizing doesn't stretch the bars.
    private func visibleSamples(for width: CGFloat) -> [MeetingAmplitudeSample] {
        guard width > 0, !samples.isEmpty else { return [] }
        let cellWidth = Self.barWidth + Self.barGap
        let maxBars = max(1, Int(width / cellWidth))
        if samples.count <= maxBars { return samples }
        return Array(samples.suffix(maxBars))
    }
}

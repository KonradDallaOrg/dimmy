import SwiftUI

// Privacy & data — telemetry toggles, anonymous identifier, feedback
// form, resource links. Mirrors Windows v3 layout. Telemetry on macOS
// today is disabled by default in the binary; the toggle here is a
// scaffolded stub awaiting the macOS telemetry FFI hookup (Phase 6).

struct MacPrivacyPage: View {
    @ObservedObject var appState: AppState
    @State private var feedbackKind: String = "general"
    @State private var feedbackText: String = ""
    @State private var feedbackEmail: String = ""
    @State private var feedbackStatus: String = ""
    /// True after a send returned -2 (telemetry disabled) → show the
    /// one-click "Enable & send" button.
    @State private var feedbackNeedsEnable: Bool = false

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            MacNote(
                title: "Your transcriptions stay yours",
                message: "Dimmy never sends your transcriptions, audio, API keys, or microphone names. Anonymous usage data and crash reports help us spot bugs and decide what to build next.",
                systemImage: "shield.fill"
            )
            .padding(.bottom, 8)

            telemetryGroup
            audioRetentionGroup
            anonymousIdGroup
            feedbackGroup
            resourcesGroup
        }
    }

    // MARK: Audio retention

    /// On-disk retention of the recorded audio. Off by default — opt
    /// in for users who want the audio next to each history row (lets
    /// them replay a past dictation). Updates round-trip through the
    /// Rust core which owns the prune thread.
    private var audioRetentionGroup: some View {
        Group {
            MacGroupLabel(text: "Audio retention")
            MacTile {
                MacRow(
                    "Save audio with each history row",
                    description: "16 kHz mono WAV in ~/Library/Application Support/dimmy/history_audio. Off by default."
                ) {
                    Toggle("", isOn: Binding(
                        get: { appState.saveAudioInHistory },
                        set: { newValue in
                            appState.saveAudioInHistory = newValue
                            persistConfig()
                        }
                    ))
                    .toggleStyle(.switch)
                    .labelsHidden()
                }
                MacRow(
                    "Keep for",
                    description: "Days before automatic delete. 0 = never auto-delete by age."
                ) {
                    Stepper(value: Binding(
                        get: { Int(appState.historyAudioKeepDays) },
                        set: { appState.historyAudioKeepDays = UInt32(max(0, $0)); persistConfig() }
                    ), in: 0...365) {
                        Text("\(appState.historyAudioKeepDays) day\(appState.historyAudioKeepDays == 1 ? "" : "s")")
                            .font(.system(size: 12))
                    }
                    .controlSize(.small)
                    .disabled(!appState.saveAudioInHistory)
                }
                MacRow(
                    "Storage cap",
                    description: "Maximum size of the history_audio folder. Oldest WAVs are deleted first when over the cap. 0 = no cap.",
                    showsDivider: false
                ) {
                    Stepper(value: Binding(
                        get: { Int(appState.historyAudioMaxMb) },
                        set: { appState.historyAudioMaxMb = UInt32(max(0, $0)); persistConfig() }
                    ), in: 0...50_000, step: 100) {
                        Text("\(appState.historyAudioMaxMb) MB")
                            .font(.system(size: 12))
                    }
                    .controlSize(.small)
                    .disabled(!appState.saveAudioInHistory)
                }
            }
            MacGroupFooter(text: "Audio is saved only when the toggle is on. Existing files stay on disk until the next prune pass.")
        }
    }

    private func persistConfig() {
        DimmyCore.shared.setConfig(appState.toRustConfig())
    }

    // MARK: Telemetry

    private var telemetryGroup: some View {
        Group {
            MacGroupLabel(text: "Telemetry")
            MacTile {
                MacRow(
                    "Send anonymous usage data",
                    description: "\"app started\", \"transcription completed in 2.3s\". No content, no identifiers."
                ) {
                    Toggle("", isOn: Binding(
                        get: { DimmyCore.shared.telemetryEnabled },
                        set: { DimmyCore.shared.telemetryEnabled = $0 }
                    ))
                    .toggleStyle(.switch)
                    .labelsHidden()
                }
                MacRow(
                    "Send crash reports",
                    description: "Stack trace only — no environment, no usernames in paths.",
                    showsDivider: false
                ) {
                    Toggle("", isOn: Binding(
                        get: { DimmyCore.shared.crashReportingEnabled },
                        set: { DimmyCore.shared.crashReportingEnabled = $0 }
                    ))
                    .toggleStyle(.switch)
                    .labelsHidden()
                }
            }
            MacGroupFooter(text: "Same pipeline as the Windows build — PostHog + Sentry, both gated by the toggles above. Off by default.")
        }
    }

    // MARK: Anonymous identifier

    private var anonymousIdGroup: some View {
        Group {
            MacGroupLabel(text: "Anonymous identifier")
            MacTile {
                MacRow(
                    "Local ID",
                    description: "Random, generated on first launch. Resetting takes effect immediately.",
                    showsDivider: false
                ) {
                    Text(anonymousIdText)
                        .font(.system(size: 11, design: .monospaced))
                        .foregroundStyle(Color.macTextSecondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                        .frame(maxWidth: 200)
                    Button("Reset") {
                        DimmyCore.shared.resetTelemetryAnonymousId()
                    }
                    .controlSize(.small)
                }
            }
        }
    }

    private var anonymousIdText: String {
        let id = DimmyCore.shared.telemetryAnonymousId
        return id.isEmpty ? "—" : id
    }

    // MARK: Feedback

    private var feedbackGroup: some View {
        Group {
            MacGroupLabel(text: "Feedback")
            MacTile {
                VStack(alignment: .leading, spacing: 10) {
                    Picker("", selection: $feedbackKind) {
                        Text("General").tag("general")
                        Text("Bug report").tag("bug")
                        Text("Idea or feature request").tag("idea")
                    }
                    .labelsHidden()
                    .frame(width: 220)

                    TextEditor(text: $feedbackText)
                        .font(.system(size: 13))
                        .frame(minHeight: 80)
                        .padding(8)
                        .background(
                            RoundedRectangle(cornerRadius: 8, style: .continuous)
                                .fill(Color.black.opacity(0.04))
                        )
                        .overlay(
                            RoundedRectangle(cornerRadius: 8, style: .continuous)
                                .stroke(Color.macControlStroke, lineWidth: 0.5)
                        )

                    TextField("Your email (optional, only if you want a reply)", text: $feedbackEmail)
                        .textFieldStyle(.plain)
                        .padding(8)
                        .background(
                            RoundedRectangle(cornerRadius: 8, style: .continuous)
                                .fill(Color.black.opacity(0.04))
                        )
                        .overlay(
                            RoundedRectangle(cornerRadius: 8, style: .continuous)
                                .stroke(Color.macControlStroke, lineWidth: 0.5)
                        )

                    HStack(spacing: 12) {
                        Button {
                            sendFeedback()
                        } label: {
                            Label("Send", systemImage: "paperplane.fill")
                        }
                        .buttonStyle(.borderedProminent)
                        .disabled(feedbackText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)

                        // Shown only after a send returned -2 (disabled):
                        // one click enables sending + retries. Explicit
                        // click = consent.
                        if feedbackNeedsEnable {
                            Button {
                                DimmyCore.shared.telemetryEnabled = true
                                feedbackNeedsEnable = false
                                sendFeedback()
                            } label: {
                                Label("Enable & send", systemImage: "paperplane.fill")
                            }
                            .buttonStyle(.borderedProminent)
                        }

                        if !feedbackStatus.isEmpty {
                            Text(feedbackStatus)
                                .font(.system(size: 11))
                                .foregroundStyle(Color.macTextSecondary)
                        }
                    }
                }
                .padding(EdgeInsets(top: 12, leading: 14, bottom: 12, trailing: 14))
            }
        }
    }

    private func sendFeedback() {
        let trimmed = feedbackText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        let trimmedEmail = feedbackEmail.trimmingCharacters(in: .whitespacesAndNewlines)
        let rc = DimmyCore.shared.captureFeedback(
            kind: feedbackKind,
            message: trimmed,
            email: trimmedEmail.isEmpty ? nil : trimmedEmail
        )
        switch rc {
        case 1:
            feedbackStatus = "Thanks! Sent."
            feedbackText = ""
            feedbackEmail = ""
            feedbackNeedsEnable = false
        case -2:
            // Telemetry disabled — offer one-click enable + send.
            feedbackStatus = "Feedback sending is off."
            feedbackNeedsEnable = true
        case -3:
            feedbackStatus = "Feedback isn't available in this build."
            feedbackNeedsEnable = false
        default:
            feedbackStatus = "Couldn't send right now. Try again later."
            feedbackNeedsEnable = false
        }
    }

    // MARK: Resources

    private var resourcesGroup: some View {
        Group {
            MacGroupLabel(text: "Resources")
            MacTile {
                MacRow(
                    "Privacy policy",
                    description: "What we collect and why"
                ) {
                    Link(destination: URL(string: "https://dimmy.app/privacy")!) {
                        Text("Open ›")
                            .font(.system(size: 12))
                    }
                }
                MacRow(
                    "What we collect",
                    description: "Line-by-line breakdown",
                    showsDivider: false
                ) {
                    Link(destination: URL(string: "https://dimmy.app/privacy#data")!) {
                        Text("Open ›")
                            .font(.system(size: 12))
                    }
                }
            }
        }
    }
}

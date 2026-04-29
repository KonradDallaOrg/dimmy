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

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            MacNote(
                title: "Your transcriptions stay yours",
                body: "Dimmy never sends your transcriptions, audio, API keys, or microphone names. Anonymous usage data and crash reports help us spot bugs and decide what to build next.",
                systemImage: "shield.fill"
            )
            .padding(.bottom, 8)

            telemetryGroup
            anonymousIdGroup
            feedbackGroup
            resourcesGroup
        }
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
                    // macOS doesn't currently surface a telemetry toggle —
                    // shipping disabled by default. Read-only for now.
                    Toggle("", isOn: .constant(false))
                        .toggleStyle(.switch)
                        .labelsHidden()
                        .disabled(true)
                }
                MacRow(
                    "Send crash reports",
                    description: "Stack trace only — no environment, no usernames in paths.",
                    showsDivider: false
                ) {
                    Toggle("", isOn: .constant(false))
                        .toggleStyle(.switch)
                        .labelsHidden()
                        .disabled(true)
                }
            }
            MacGroupFooter(text: "Telemetry on macOS is currently disabled in the binary. The Windows build has it wired through PostHog; macOS parity is on the roadmap.")
        }
    }

    // MARK: Anonymous identifier

    private var anonymousIdGroup: some View {
        Group {
            MacGroupLabel(text: "Anonymous identifier")
            MacTile {
                MacRow(
                    "Local ID",
                    description: "Random, generated on first launch. Resetting takes effect after restart.",
                    showsDivider: false
                ) {
                    Text(anonymousIdText)
                        .font(.system(size: 11, design: .monospaced))
                        .foregroundStyle(Color.macTextSecondary)
                    Button("Reset") {
                        // Placeholder until macOS telemetry FFI ships.
                    }
                    .controlSize(.small)
                    .disabled(true)
                }
            }
        }
    }

    private var anonymousIdText: String {
        // Stand-in. Once macOS telemetry lands, read from
        // DimmyCore.shared.getAnonymousId().
        "—"
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
                            // Phase 6: FFI to dimmy_telemetry_capture_feedback.
                            feedbackStatus = "Feedback sending isn't wired on macOS yet."
                        } label: {
                            Label("Send", systemImage: "paperplane.fill")
                        }
                        .buttonStyle(.borderedProminent)
                        .disabled(feedbackText.isEmpty)

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

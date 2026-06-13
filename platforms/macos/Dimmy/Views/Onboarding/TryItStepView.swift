import SwiftUI

struct TryItStepView: View {
    @ObservedObject var appState: AppState
    let onComplete: () -> Void

    @State private var demoText: String = ""
    @State private var hasTriedRecording = false
    @State private var showSuccess = false
    @State private var modelReady: Bool = false
    // Live status shown in the box while there's no result yet, so the hold
    // visibly works ("Listening...") and the post-release STT round-trip
    // ("Transcribing...") doesn't look frozen. Mirrors the Windows Try-it.
    @State private var trialStatus: String = ""

    private var needsCloudKey: Bool {
        appState.sttMode == "cloud" && !appState.hasKey
    }
    private var needsLocalModel: Bool {
        appState.sttMode == "local" && !modelReady
    }
    private var needsSetup: Bool {
        needsCloudKey || needsLocalModel
    }

    // Mode-aware verb: PTT is "Hold", toggle is "Press". The old copy was
    // hardcoded "Hold", which lied in toggle mode.
    private var verb: String {
        appState.preferredMode == .pushToTalk ? "Hold" : "Press"
    }

    var body: some View {
        VStack(spacing: 16) {
            Spacer(minLength: 4)

            if showSuccess {
                successView
            } else {
                tryView
            }

            Spacer(minLength: 4)
        }
        .padding(.horizontal, 32)
        .onAppear {
            modelReady = appState.localSttBackend == "parakeet"
                ? DimmyCore.shared.parakeetBundlePresent()
                : DimmyCore.shared.modelExists(appState.localModel)
        }
        .onChange(of: appState.recordingState) { _, newState in
            // Show the transcript and a result marker, but DON'T auto-jump to
            // the success screen — the user reads what came out, then advances
            // with Continue. Auto-skipping on .idle hid the result.
            switch newState {
            case .recording:
                trialStatus = "Listening..."
            case .transcribing, .processing:
                trialStatus = "Transcribing..."
            case .completing:
                demoText = appState.lastTranscript.isEmpty ? "No speech detected" : appState.lastTranscript
                hasTriedRecording = true
                trialStatus = ""
            case .idle:
                trialStatus = ""
            }
        }
    }

    private var tryView: some View {
        VStack(spacing: 16) {
            Text("Try it!")
                .font(.system(size: 26, weight: .bold))

            if needsSetup {
                setupCard
            } else {
                readyView
            }

            if hasTriedRecording && !needsSetup {
                // The user has seen a transcript and explicitly advances.
                Button(action: {
                    withAnimation(.spring(response: 0.4)) { showSuccess = true }
                }) {
                    Text("Continue")
                        .font(.system(size: 14, weight: .semibold))
                        .frame(maxWidth: 200)
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
            } else {
                Button(action: {
                    withAnimation(.spring(response: 0.4)) { showSuccess = true }
                }) {
                    Text(needsSetup ? "Finish (I'll set up later)" : "Skip for now")
                        .font(.system(size: 12))
                        .foregroundColor(.secondary)
                }
                .buttonStyle(.plain)
            }
        }
    }

    private var readyView: some View {
        VStack(spacing: 14) {
            Text("\(verb) \(appState.shortcut.displayString) and say something")
                .font(.system(size: 14))
                .foregroundColor(.secondary)

            Text("The pill overlay will animate while you speak")
                .font(.system(size: 12))
                .foregroundColor(Color(nsColor: .tertiaryLabelColor))

            VStack(alignment: .leading, spacing: 6) {
                Text("Your dictation will appear here:")
                    .font(.system(size: 11))
                    .foregroundColor(Color(nsColor: .tertiaryLabelColor))

                ZStack(alignment: .topLeading) {
                    RoundedRectangle(cornerRadius: 8)
                        .fill(Color(nsColor: .textBackgroundColor))
                        .frame(height: 80)

                    if demoText.isEmpty {
                        Text(trialStatus.isEmpty ? "Waiting for your voice..." : trialStatus)
                            .font(.system(size: 13))
                            .foregroundColor(Color(nsColor: .tertiaryLabelColor))
                            .padding(10)
                    } else {
                        Text(demoText)
                            .font(.system(size: 13))
                            .textSelection(.enabled)
                            .padding(10)
                    }
                }
                .overlay(
                    RoundedRectangle(cornerRadius: 8)
                        .stroke(Color.primary.opacity(0.1), lineWidth: 1)
                )
            }

            if hasTriedRecording {
                HStack(spacing: 6) {
                    Image(systemName: "checkmark.circle.fill")
                        .foregroundColor(.green)
                        .font(.system(size: 14))
                    Text("Transcribed. Looks right?")
                        .font(.system(size: 12))
                        .foregroundColor(.secondary)
                }
            }
        }
    }

    private var setupCard: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                Image(systemName: "gearshape.fill")
                    .foregroundColor(.accentColor)
                Text("One more thing")
                    .font(.system(size: 14, weight: .semibold))
            }

            if needsCloudKey {
                Text("Dimmy is configured for cloud transcription. Add an API key in Settings to start dictating.")
                    .font(.system(size: 12))
                    .foregroundColor(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                Button("Open Settings") {
                    AppDelegate.shared?.openSettings()
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.regular)
            } else if needsLocalModel {
                Text("No local model is on disk yet. Pick one from Settings → Voice → Local model to start dictating.")
                    .font(.system(size: 12))
                    .foregroundColor(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                Button("Open Settings") {
                    AppDelegate.shared?.openSettings()
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.regular)
            }
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: 12)
                .fill(Color(nsColor: .controlBackgroundColor))
        )
        .overlay(
            RoundedRectangle(cornerRadius: 12)
                .stroke(Color.accentColor.opacity(0.2), lineWidth: 1)
        )
    }

    private var successView: some View {
        VStack(spacing: 20) {
            Image(systemName: "checkmark.circle.fill")
                .font(.system(size: 56))
                .foregroundColor(.green)

            Text("You're all set!")
                .font(.system(size: 24, weight: .bold))

            Text("Dimmy lives in your menu bar.\n\(verb) \(appState.shortcut.displayString) anywhere to dictate.")
                .font(.system(size: 13))
                .foregroundColor(.secondary)
                .multilineTextAlignment(.center)
                .lineSpacing(4)

            Button(action: {
                appState.showPillIntro = true
                onComplete()
            }) {
                Text("Start Using Dimmy")
                    .font(.system(size: 14, weight: .semibold))
                    .frame(maxWidth: 200)
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.large)
        }
    }

}

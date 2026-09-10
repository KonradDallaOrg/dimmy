import AppKit
import SwiftUI

/// Three focused steps to get Command mode working: bind a shortcut, confirm
/// the model answers, then use it for real.
///
/// Mirror of `CommandWizardWindow` on Windows, and like it a standalone window
/// rather than extra steps inside `OnboardingContainerView` — whose count is
/// pinned by `SelfTests.testOnboardingStepCount`, which `fatalError`s the app
/// at launch.
///
/// One thing genuinely differs from Windows, and it is not a shortcut taken:
/// the Mac's command hotkey does not go through the Rust hook at all. It runs
/// on the app's own `CGEventTap` (`HotkeyManager.commandComboState`), fed from
/// `AppState.commandHotkey`, which `HotkeyManager` observes. So this wizard
/// sets that property and the binding follows; there is no FFI call to make.
struct CommandWizardView: View {
    @ObservedObject var appState: AppState
    let onFinish: () -> Void

    @State private var step = 0
    @State private var recording = false
    @State private var testing = false
    @State private var testResult: (ok: Bool, text: String)?
    @State private var trialResult: String?
    @State private var trialError: String?

    private static let totalSteps = 3

    /// A work-shaped paragraph. "Hello world" proves the wiring and teaches
    /// nothing about what the feature is for.
    private static let sample =
        "allora per il progetto NFC dobbiamo decidere chi fa la presentazione, "
        + "poi c'e' il tema dei costi che non abbiamo ancora chiuso, e Jasmine "
        + "voleva sapere se il QR code resta o lo togliamo del tutto"

    private static let suggestions = [
        "\"fammi lo schema\"",
        "\"riassumilo in tre punti\"",
        "\"riscrivilo in modo formale\"",
    ]

    @State private var spoken = ""
    @State private var suggestion = Self.suggestions.randomElement() ?? ""

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            header
            Divider()
            ScrollView {
                Group {
                    switch step {
                    case 0: shortcutStep
                    case 1: modelStep
                    default: tryStep
                    }
                }
                .padding(.vertical, 4)
            }
            .frame(minHeight: 240)
            footer
        }
        .padding(28)
        .frame(minWidth: 520, minHeight: 460)
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text("Step \(step + 1) of \(Self.totalSteps)")
                .font(.system(size: 11)).foregroundStyle(.secondary)
            Text(title).font(.system(size: 22, weight: .semibold))
            if !subtitle.isEmpty {
                Text(subtitle).font(.system(size: 12)).foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    private var title: String {
        switch step {
        case 0: return "Pick a shortcut"
        case 1: return "Check the model"
        default: return "Try it"
        }
    }

    private var subtitle: String {
        switch step {
        case 0: return "Select some text anywhere, hold this shortcut, and say what to do with it."
        case 1: return "Command mode uses your dictation LLM, not the recap one."
        default: return ""
        }
    }

    // ── Step 0 ──────────────────────────────────────────────────────

    private var shortcutStep: some View {
        VStack(alignment: .leading, spacing: 12) {
            MacTile {
                MacRow("Command shortcut",
                       description: appState.commandHotkey?.displayString ?? "Not set",
                       icon: "text.cursor",
                       iconBackground: Color(red: 0.55, green: 0.35, blue: 0.95),
                       showsDivider: false) {
                    Button(appState.commandHotkey == nil ? "Record" : "Change") {
                        recording = true
                    }
                }
            }
            Text("A letter with at least one modifier, for example ⌃⌥C, or two "
                 + "modifiers on their own. It applies to the NEXT dictation only, "
                 + "and follows the push-to-talk or toggle behaviour you already set.")
                .font(.system(size: 11)).foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
        // Reuses the recorder from Settings rather than adding a FIFTH copy of
        // the NSEvent monitor dance: the conflict checks against the other
        // hotkeys already live in there.
        .sheet(isPresented: $recording) {
            CommandHotkeyRecorderSheet(appState: appState, isPresented: $recording)
        }
    }

    // ── Step 1 ──────────────────────────────────────────────────────

    private var modelStep: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(modelSummary).fixedSize(horizontal: false, vertical: true)
            HStack(spacing: 10) {
                Button("Test the model") { runModelTest() }
                    .buttonStyle(.borderedProminent)
                    .disabled(testing)
                Button("Open or change in Settings") {
                    AppDelegate.shared?.openSettings(at: .output)
                }
                if testing { ProgressView().controlSize(.small) }
            }
            if let r = testResult {
                MacNote(title: r.ok ? "The model answered" : "The call failed",
                        message: r.text,
                        systemImage: r.ok ? "checkmark.circle.fill" : "exclamationmark.triangle.fill")
            }
        }
    }

    /// Reads the config the core owns. Command mode dispatches through the
    /// DICTATION LLM config, not `recap_model_override` — testing the wrong
    /// one would be worse than not testing.
    private var modelSummary: String {
        guard let cfg = DimmyCore.shared.getConfig() else {
            return "Could not read the configuration."
        }
        let mode = (cfg["llm_mode"] as? String) ?? "cloud"
        let key = mode == "local" ? "local_llm_model" : "llm_api_model"
        let model = (cfg[key] as? String) ?? ""
        if model.isEmpty {
            return "No LLM model is configured yet. Set one in Settings, then come back."
        }
        return "Using \(mode == "local" ? "the local model" : "the cloud model") \(model)."
    }

    private func runModelTest() {
        testing = true
        testResult = nil
        DispatchQueue.global(qos: .userInitiated).async {
            let result = DimmyCore.shared.llmCallRaw(
                prompt: "Reply with exactly: ok", modelOverride: "", maxTokens: 16)
            DispatchQueue.main.async {
                testing = false
                switch result {
                case .success(let text):
                    testResult = (true, text.trimmingCharacters(in: .whitespacesAndNewlines))
                case .failure(let err):
                    testResult = (false, "\(err)")
                }
            }
        }
    }

    // ── Step 2 ──────────────────────────────────────────────────────

    /// Windows can let the real hotkey fire here, because its wizard window IS
    /// the focused app and the command path reads the focused selection through
    /// UIA. On the Mac the selection is read through the Accessibility API from
    /// whatever app is frontmost, which during a wizard is the wizard — so
    /// driving the real hotkey would read this window's own text and prove
    /// nothing extra. Instead the transform is called directly with the sample
    /// and the typed instruction: the LLM leg, the prompt and the rc contract
    /// are all the shipping ones, and the shortcut was already exercised in
    /// step 0 by recording it.
    private var tryStep: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("This is the text to work on:")
                .font(.system(size: 12)).foregroundStyle(.secondary)
            Text(Self.sample)
                .font(.system(size: 12))
                .padding(10)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(RoundedRectangle(cornerRadius: 8).fill(Color.primary.opacity(0.05)))
            HStack(spacing: 6) {
                Text("Ask for something, e.g.").font(.system(size: 11)).foregroundStyle(.secondary)
                Text(suggestion).font(.system(size: 11, weight: .semibold))
            }
            HStack(spacing: 10) {
                TextField("what to do with it", text: $spoken)
                    .textFieldStyle(.roundedBorder)
                Button("Run") { runTransform() }
                    .buttonStyle(.borderedProminent)
                    .disabled(spoken.trimmingCharacters(in: .whitespaces).isEmpty || testing)
                if testing { ProgressView().controlSize(.small) }
            }
            if let err = trialError {
                MacNote(title: "It did not run", message: err,
                        systemImage: "exclamationmark.triangle.fill")
            }
            if let out = trialResult {
                ScrollView {
                    Text(out).font(.system(size: 12)).textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                .frame(height: 110)
                .padding(8)
                .background(RoundedRectangle(cornerRadius: 8).fill(Color.primary.opacity(0.05)))
            }
        }
    }

    private func runTransform() {
        testing = true
        trialResult = nil
        trialError = nil
        let instruction = spoken
        DispatchQueue.global(qos: .userInitiated).async {
            let (text, rc) = DimmyCore.shared.commandTransform(
                selection: Self.sample, spoken: instruction)
            DispatchQueue.main.async {
                testing = false
                if let text, !text.isEmpty {
                    trialResult = text
                } else {
                    // The rc contract is documented on dimmy_command_transform;
                    // naming the two that a user can act on beats a bare number.
                    trialError = rc == -3
                        ? "No API key for the LLM. Add one in Settings, Providers."
                        : rc == -4
                        ? "The local model is not on disk. Download it in Settings."
                        : "The transform returned nothing (rc \(rc))."
                }
            }
        }
    }

    // ── Footer ──────────────────────────────────────────────────────

    private var footer: some View {
        HStack {
            if step > 0 {
                Button("Back") { step -= 1 }
            }
            Spacer()
            if step == 1 {
                Button("Skip") { step = Self.totalSteps - 1 }
            }
            Button(step == Self.totalSteps - 1 ? "Done" : "Continue") {
                if step == 0 && appState.commandHotkey == nil { return }
                if step >= Self.totalSteps - 1 { onFinish() } else { step += 1 }
            }
            .buttonStyle(.borderedProminent)
            .disabled(step == 0 && appState.commandHotkey == nil)
        }
    }
}

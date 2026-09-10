import AppKit
import SwiftUI

/// Four focused steps to get Meeting mode working: bind a shortcut, confirm
/// the recap model answers, decide where recaps go, then record one.
///
/// Mirror of `MeetingWizardWindow` on Windows. Step 2 is the reason this has
/// four steps and Command has three: a recap nobody can find is a recap that
/// did not happen, and both destinations were otherwise buried in Settings.
///
/// Like the Command wizard, the meeting hotkey here never reaches the Rust
/// core — the Mac runs it on its own `CGEventTap` from `AppState.meetingHotkey`
/// (see `HotkeyManager.meetingComboState`), so setting the property is the
/// whole binding.
struct MeetingWizardView: View {
    @ObservedObject var appState: AppState
    let onFinish: () -> Void

    @State private var step = 0
    @State private var recording = false
    @State private var testing = false
    @State private var testResult: (ok: Bool, text: String)?
    @State private var chunkCount = 0
    @State private var liveText = ""
    @State private var folderError: String?

    /// Mac-only preference, UserDefaults-backed, deliberately not a Rust
    /// config field — same key `MeetingPostProcessService` reads.
    @AppStorage("recapExportFolder") private var exportFolder = ""

    private static let totalSteps = 4

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            header
            Divider()
            ScrollView {
                Group {
                    switch step {
                    case 0: shortcutStep
                    case 1: modelStep
                    case 2: destinationStep
                    default: tryStep
                    }
                }
                .padding(.vertical, 4)
            }
            .frame(minHeight: 260)
            footer
        }
        .padding(28)
        .frame(minWidth: 560, minHeight: 500)
        // The real meeting's own counters. If the hotkey, the microphone, the
        // loopback or the STT is broken, nothing moves here and the user finds
        // out during the wizard rather than after a real meeting.
        .onReceive(appState.$meetingChunkCount) { count in
            guard count > 0 else { return }
            chunkCount = count
        }
        .onReceive(appState.$meetingLiveTranscript) { text in
            liveText = text
        }
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
        case 1: return "Check the recap model"
        case 2: return "Where the recap goes"
        default: return "Record a real one"
        }
    }

    private var subtitle: String {
        switch step {
        case 0: return "One key combination starts and stops a meeting from anywhere."
        case 1: return "The recap is the whole point of a meeting recording."
        case 2: return "Both are optional. The recap is always saved with the meeting."
        default:
            let combo = appState.meetingHotkey?.displayString ?? "your shortcut"
            return "Press \(combo), say a few sentences, press it again."
        }
    }

    // ── Step 0 ──────────────────────────────────────────────────────

    private var shortcutStep: some View {
        VStack(alignment: .leading, spacing: 12) {
            MacTile {
                MacRow("Meeting shortcut",
                       description: appState.meetingHotkey?.displayString ?? "Not set",
                       icon: "waveform",
                       iconBackground: Color(red: 0.94, green: 0.36, blue: 0.24),
                       showsDivider: false) {
                    Button(appState.meetingHotkey == nil ? "Record" : "Change") {
                        recording = true
                    }
                }
            }
            Text("Press it once to start a meeting, again to stop. A meeting cannot be push-to-talk.")
                .font(.system(size: 11)).foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
        .sheet(isPresented: $recording) {
            MeetingHotkeyRecorderSheet(appState: appState, isPresented: $recording)
        }
    }

    // ── Step 1 ──────────────────────────────────────────────────────

    private var modelStep: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(recapModel.label).fixedSize(horizontal: false, vertical: true)
            HStack(spacing: 10) {
                Button("Test the recap model") { runModelTest() }
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

    /// `recap_model_override` wins when set; empty means the recap inherits
    /// the main LLM config. Worth showing rather than hiding behind one label,
    /// because they are genuinely two settings.
    private var recapModel: (label: String, override: String) {
        guard let cfg = DimmyCore.shared.getConfig() else {
            return ("Could not read the configuration.", "")
        }
        let over = (cfg["recap_model_override"] as? String) ?? ""
        if !over.isEmpty { return ("Recaps use \(over).", over) }
        let mode = (cfg["llm_mode"] as? String) ?? "cloud"
        let key = mode == "local" ? "local_llm_model" : "llm_api_model"
        let model = (cfg[key] as? String) ?? ""
        return (model.isEmpty
            ? "No LLM model is configured yet. Set one in Settings, then come back."
            : "Recaps inherit your main model, \(model).", "")
    }

    private func runModelTest() {
        testing = true
        testResult = nil
        let over = recapModel.override
        DispatchQueue.global(qos: .userInitiated).async {
            let result = DimmyCore.shared.llmCallRaw(
                prompt: "Reply with exactly: ok", modelOverride: over, maxTokens: 16)
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

    private var destinationStep: some View {
        VStack(alignment: .leading, spacing: 14) {
            MacTile {
                MacRow("Notion",
                       description: appState.hasNotionToken
                           ? "Connected. Recaps can be pushed to a page or database."
                           : "Not connected. Uses your own Notion integration token.",
                       icon: "doc.text",
                       iconBackground: Color(red: 0.15, green: 0.15, blue: 0.18)) {
                    Button("Open Notion settings") {
                        AppDelegate.shared?.openSettings(at: .integrations)
                    }
                }
                MacRow("Send every recap automatically",
                       icon: "paperplane.fill",
                       iconBackground: Color(red: 0.04, green: 0.52, blue: 1.00),
                       showsDivider: false) {
                    Toggle("", isOn: Binding(
                        get: { appState.notionAutoSend },
                        set: { newValue in
                            appState.notionAutoSend = newValue
                            // Single-writer rule: the host asks the core to
                            // save, it never writes config.json itself.
                            // `notion_auto_send` is one of the fields
                            // toRustConfig always emits (see its docstring:
                            // only the notion TARGET ids are gated).
                            DimmyCore.shared.setConfig(appState.toRustConfig())
                        }))
                        .labelsHidden()
                        .toggleStyle(.switch)
                        .disabled(!appState.hasNotionToken)
                }
            }
            MacTile {
                MacRow("A folder on this Mac",
                       description: exportFolder.isEmpty
                           ? "Writes recap.md next to your notes. Point it at a synced folder and the recap lands there by itself."
                           : exportFolder,
                       icon: "folder",
                       iconBackground: Color(red: 0.35, green: 0.55, blue: 0.95),
                       showsDivider: false) {
                    Button(exportFolder.isEmpty ? "Choose" : "Change") { pickFolder() }
                }
            }
            if let folderError {
                MacNote(title: "That folder will not work", message: folderError,
                        systemImage: "exclamationmark.triangle.fill")
            }
        }
    }

    private func pickFolder() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.canCreateDirectories = true
        panel.prompt = "Use this folder"
        panel.message = "Where should recaps be written?"
        if !exportFolder.isEmpty {
            panel.directoryURL = URL(fileURLWithPath: exportFolder)
        }
        guard panel.runModal() == .OK, let url = panel.url else { return }
        // Same probe Settings uses. Accepting a folder we cannot write to
        // would make every later recap vanish silently, which is the one
        // outcome this whole step exists to prevent.
        guard MeetingPostProcessService.isExportFolderWritable(url.path) else {
            folderError = "Cannot write to \(url.lastPathComponent). Pick a folder you have write access to."
            return
        }
        folderError = nil
        exportFolder = url.path
    }

    // ── Step 3 ──────────────────────────────────────────────────────

    private var tryStep: some View {
        VStack(alignment: .leading, spacing: 12) {
            MacNote(title: chunkCount > 0 ? "It works" : "Waiting for you to start",
                    message: chunkCount > 0
                        ? "\(chunkCount) chunk(s) transcribed. Press the shortcut again to stop and generate the recap."
                        : "Press your meeting shortcut and talk for a few seconds.",
                    systemImage: chunkCount > 0 ? "checkmark.circle.fill" : "mic")
            Text("The first time you start a meeting Dimmy shows a consent notice and announces "
                 + "that the meeting is being recorded. That is deliberate, and it will appear now.")
                .font(.system(size: 11)).foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            if !liveText.isEmpty {
                ScrollView {
                    Text(liveText).font(.system(size: 11, design: .monospaced))
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                .frame(height: 130)
                .padding(8)
                .background(RoundedRectangle(cornerRadius: 8).fill(Color.primary.opacity(0.05)))
            }
        }
    }

    // ── Footer ──────────────────────────────────────────────────────

    private var footer: some View {
        HStack {
            if step > 0 { Button("Back") { step -= 1 } }
            Spacer()
            if step == 1 || step == 2 {
                Button("Skip") { step = Self.totalSteps - 1 }
            }
            Button(step == Self.totalSteps - 1 ? "Done" : "Continue") {
                if step >= Self.totalSteps - 1 { onFinish() } else { step += 1 }
            }
            .buttonStyle(.borderedProminent)
            .disabled(step == 0 && appState.meetingHotkey == nil)
        }
    }
}

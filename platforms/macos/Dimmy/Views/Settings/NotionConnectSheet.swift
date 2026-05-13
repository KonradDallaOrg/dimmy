import SwiftUI

/// 3-step Notion connection wizard, modal Sheet.
/// Mac mirror of `Views/NotionConnectDialog.xaml` on Windows.
///
/// Flow:
///   Step 1 — instructions + "Open Notion" link (no input).
///   Step 2 — paste token, Verify (FFI ping). Next blocked until ✓.
///   Step 3 — Refresh list, pick destination from Picker.
///
/// Re-runnable: callers pass `initialStep` — 1 = full setup,
/// 3 = "change destination" (jumps past prepare + token, refreshes
/// list immediately on entry).
///
/// Persistence: token via `notionSetToken` (encrypted keystore).
/// Target via `setConfig(toRustConfig)` round-trip — same single-
/// writer rule as the rest of Dimmy.
struct NotionConnectSheet: View {
    @ObservedObject var appState: AppState
    let initialStep: Int
    /// Caller closes the sheet by flipping the parent's binding.
    let onClose: () -> Void

    @Environment(\.dismiss) private var dismiss

    @State private var currentStep: Int = 1
    @State private var tokenInput: String = ""
    @State private var tokenVerified: Bool = false
    @State private var verifyStatus: String = ""
    @State private var verifyIsError: Bool = false
    @State private var verifying: Bool = false
    @State private var workspaceName: String = ""

    @State private var searchResults: [SearchResult] = []
    @State private var refreshing: Bool = false
    @State private var refreshStatus: String = ""
    @State private var pickedTargetId: String = ""
    @State private var pickedTargetKind: String = ""
    @State private var pickedTargetTitle: String = ""

    struct SearchResult: Identifiable, Hashable {
        let id: String
        let object: String
        let title: String
    }

    var body: some View {
        VStack(spacing: 16) {
            header
            progressDots
            Divider()

            ScrollView {
                Group {
                    switch currentStep {
                    case 1: stepOne
                    case 2: stepTwo
                    case 3: stepThree
                    default: EmptyView()
                    }
                }
                .padding(.vertical, 4)
            }
            .frame(minHeight: 280)

            Divider()
            footer
        }
        .padding(20)
        .frame(width: 540)
        .onAppear {
            currentStep = max(1, min(3, initialStep))
            // Re-runnable: when caller jumps to step 3 we treat the
            // existing token as already verified (it was verified in
            // a prior run) and auto-populate destination list.
            if currentStep == 3 {
                tokenVerified = true
                Task { await refreshAsync() }
            }
        }
    }

    // MARK: - Header / progress / footer

    private var header: some View {
        HStack(spacing: 10) {
            Image("notion")
                .resizable()
                .scaledToFit()
                .frame(width: 22, height: 22)
            Text("Connect Notion")
                .font(.system(size: 16, weight: .semibold))
            Spacer()
        }
    }

    private var progressDots: some View {
        HStack(spacing: 8) {
            ForEach(1...3, id: \.self) { i in
                Circle()
                    .fill(i <= currentStep ? Color.accentColor : Color.gray.opacity(0.3))
                    .frame(width: 8, height: 8)
            }
        }
        .frame(maxWidth: .infinity)
    }

    private var footer: some View {
        HStack {
            Button("Cancel") { onClose() }
                .keyboardShortcut(.cancelAction)
            Spacer()
            if currentStep > 1 {
                Button("Back") { currentStep -= 1 }
            }
            Button(currentStep == 3 ? "Done" : "Next") {
                if currentStep < 3 {
                    currentStep += 1
                    if currentStep == 3 && searchResults.isEmpty {
                        Task { await refreshAsync() }
                    }
                } else {
                    finish()
                }
            }
            .buttonStyle(.borderedProminent)
            .disabled(!canProceed)
            .keyboardShortcut(.defaultAction)
        }
    }

    private var canProceed: Bool {
        switch currentStep {
        case 1: return true
        case 2: return tokenVerified
        case 3: return !pickedTargetId.isEmpty
        default: return false
        }
    }

    // MARK: - Steps

    private var stepOne: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("1 — Prepare your Notion workspace")
                .font(.system(size: 16, weight: .semibold))
            Text("Dimmy uploads recaps via a private Notion integration. You'll do this once.")
                .font(.system(size: 12))
                .foregroundStyle(.secondary)

            VStack(alignment: .leading, spacing: 10) {
                instructionRow("a.", "Open the Notion integrations page (button below).")
                instructionRow("b.", "Click ‘New integration’, name it ‘Dimmy’, pick your workspace, save.")
                instructionRow("c.", "Copy the Internal Integration Secret — that's your token.")
                instructionRow("d.", "Open the Notion page (or database) you want recaps to land in. Click ··· → Connections → add your new integration.")
            }
            .padding(14)
            .background(
                RoundedRectangle(cornerRadius: 6)
                    .fill(Color(NSColor.controlBackgroundColor))
            )
            .overlay(
                RoundedRectangle(cornerRadius: 6)
                    .stroke(Color.gray.opacity(0.25), lineWidth: 1)
            )

            Button("Open Notion integrations") {
                if let url = URL(string: "https://www.notion.so/my-integrations") {
                    NSWorkspace.shared.open(url)
                }
            }

            Text("When you have the token copied and the page connected, click Next.")
                .font(.system(size: 11))
                .foregroundStyle(.secondary)
        }
    }

    private func instructionRow(_ marker: String, _ text: String) -> some View {
        HStack(alignment: .top, spacing: 8) {
            Text(marker).font(.system(size: 13, weight: .semibold))
            Text(text).font(.system(size: 13))
            Spacer(minLength: 0)
        }
    }

    private var stepTwo: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("2 — Paste your token")
                .font(.system(size: 16, weight: .semibold))
            Text("Tokens stay on this device, encrypted in Dimmy's local keystore. They never appear in config.json.")
                .font(.system(size: 12))
                .foregroundStyle(.secondary)

            SecureField("ntn_xxxxxxxxxxxxxxxx", text: $tokenInput)
                .textFieldStyle(.roundedBorder)
                .onChange(of: tokenInput) { _, _ in
                    // Editing invalidates any prior verification.
                    if tokenVerified {
                        tokenVerified = false
                        verifyStatus = ""
                    }
                }

            HStack(spacing: 10) {
                Button("Verify") {
                    Task { await verifyAsync() }
                }
                .buttonStyle(.bordered)
                .disabled(verifying || tokenInput.trimmingCharacters(in: .whitespaces).isEmpty)

                if verifying {
                    ProgressView().controlSize(.small)
                } else if tokenVerified {
                    Image(systemName: "checkmark.circle.fill")
                        .foregroundStyle(.green)
                }

                Text(verifyStatus)
                    .font(.system(size: 12))
                    .foregroundStyle(verifyIsError ? Color.red : .secondary)
                    .lineLimit(2)
                Spacer()
            }
        }
    }

    private var stepThree: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("3 — Pick a destination")
                .font(.system(size: 16, weight: .semibold))
            Text("Dimmy lists every page or database your integration can see. If your page isn't here, open it in Notion → ··· → Connections → add your integration, then refresh.")
                .font(.system(size: 12))
                .foregroundStyle(.secondary)

            HStack(spacing: 10) {
                Button(refreshing ? "Refreshing…" : "Refresh list") {
                    Task { await refreshAsync() }
                }
                .disabled(refreshing)

                if refreshing { ProgressView().controlSize(.small) }

                Text(refreshStatus)
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
                Spacer()
            }

            Picker("Destination", selection: $pickedTargetId) {
                Text("— pick one —").tag("")
                ForEach(searchResults) { r in
                    Text(displayLabel(for: r)).tag(r.id)
                }
            }
            .labelsHidden()
            .frame(maxWidth: .infinity, alignment: .leading)
            .onChange(of: pickedTargetId) { _, newValue in
                if let r = searchResults.first(where: { $0.id == newValue }) {
                    pickedTargetKind = r.object == "database" ? "database" : "page"
                    pickedTargetTitle = r.title
                }
            }
        }
    }

    private func displayLabel(for r: SearchResult) -> String {
        let title = r.title.isEmpty ? "(untitled)" : r.title
        switch r.object {
        case "database": return "\(title) — database"
        case "page":     return "\(title) — page"
        default:         return title
        }
    }

    // MARK: - Actions

    private func verifyAsync() async {
        let token = tokenInput.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !token.isEmpty else { return }
        verifyStatus = "Pinging Notion…"
        verifyIsError = false
        verifying = true
        let saved = DimmyCore.shared.notionSetToken(token)
        guard saved else {
            verifyStatus = "Couldn't save the token to local storage."
            verifyIsError = true
            verifying = false
            return
        }
        let json = await Task.detached { DimmyCore.shared.notionTestConnection() }.value
        verifying = false
        guard let json = json,
              let data = json.data(using: .utf8),
              let dict = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            verifyIsError = true
            verifyStatus = "Invalid response from Notion."
            return
        }
        let ok = dict["ok"] as? Bool ?? false
        if ok {
            let bot = dict["bot_name"] as? String ?? "Dimmy"
            let ws = dict["workspace_name"] as? String ?? ""
            workspaceName = ws
            tokenVerified = true
            appState.hasNotionToken = true
            verifyIsError = false
            verifyStatus = "Connected as “\(bot)” in “\(ws)”."
        } else {
            tokenVerified = false
            verifyIsError = true
            verifyStatus = (dict["error"] as? String) ?? "Connection failed."
        }
    }

    private func refreshAsync() async {
        refreshing = true
        refreshStatus = "Loading…"
        let json = await Task.detached { DimmyCore.shared.notionSearch("") }.value
        refreshing = false
        guard let json = json, let data = json.data(using: .utf8) else {
            refreshStatus = "Couldn't load list from Notion."
            return
        }
        if let dict = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
           let err = dict["error"] as? String {
            refreshStatus = "Couldn't load list: \(err)"
            return
        }
        guard let arr = try? JSONSerialization.jsonObject(with: data) as? [[String: Any]] else {
            refreshStatus = "Couldn't parse Notion response."
            return
        }
        searchResults = arr.compactMap { d in
            guard let id = d["id"] as? String, !id.isEmpty else { return nil }
            return SearchResult(
                id: id,
                object: d["object"] as? String ?? "",
                title: d["title"] as? String ?? ""
            )
        }
        // Pre-select existing target on re-run.
        if pickedTargetId.isEmpty, !appState.notionTargetId.isEmpty,
           searchResults.contains(where: { $0.id == appState.notionTargetId }) {
            pickedTargetId = appState.notionTargetId
            if let r = searchResults.first(where: { $0.id == pickedTargetId }) {
                pickedTargetKind = r.object == "database" ? "database" : "page"
                pickedTargetTitle = r.title
            }
        }
        refreshStatus = searchResults.isEmpty
            ? "Nothing visible yet. Open a Notion page → ··· → Connections → add Dimmy. Then Refresh again."
            : "Found \(searchResults.count) item(s)."
    }

    private func finish() {
        guard !pickedTargetId.isEmpty else { return }
        appState.notionTargetId = pickedTargetId
        appState.notionTargetKind = pickedTargetKind
        appState.notionTargetTitle = pickedTargetTitle
        // includeNotion: true — this is one of the only two sites
        // (picker + Disconnect) that owns explicit "set/clear"
        // intent for the Notion destination. Generic Settings
        // saves omit these fields so a transient empty AppState
        // never wipes the disk value. See AppState.toRustConfig.
        DimmyCore.shared.setConfig(appState.toRustConfig(includeNotion: true))
        onClose()
    }
}

import AppKit
import SwiftUI

/// Confluence connection sheet: credentials, then destination.
///
/// Mac mirror of `ConfluenceConnectDialog` on Windows, and like it two steps
/// rather than the Notion wizard's three — an Atlassian API token needs no
/// integration to be created and shared first, so "prepare" and "paste"
/// collapse into one screen.
///
/// Three things here are not arbitrary; each one cost a round of debugging on
/// Windows and is reproduced deliberately:
///
/// - **Continue is derived, never assigned.** Step 1 needs a checked
///   credential, step 2 a chosen space. Setting it at each transition lost the
///   race with whatever raised a field event afterwards, and showed a dead
///   button with a space plainly selected.
/// - **The space list has a filter.** A real tenant returned 530 spaces, and a
///   scoped token cannot identify the user at all (there is no user scope
///   among the granular v2 ones), so their own space is somewhere in that list
///   rather than preselected.
/// - **Site and email are loaded on open**, not only when Check runs. Entering
///   at step 2 to change the space skips Check, and saving would otherwise
///   write empty strings over a working configuration.
struct ConfluenceConnectSheet: View {
    @ObservedObject var appState: AppState
    /// 1 = full setup, 2 = change destination only (credentials already good).
    let initialStep: Int
    let onClose: (_ completed: Bool) -> Void

    @State private var step: Int
    @State private var site: String = ""
    @State private var email: String = ""
    @State private var token: String = ""
    @State private var verified = false
    @State private var busy = false
    @State private var status: (ok: Bool, text: String)?

    @State private var spaces: [ConfluenceSpace] = []
    @State private var filter: String = ""
    @State private var pickedId: String = ""
    @State private var loadingSpaces = false

    /// Whether a token is already stored. Drives the copy on the token field:
    /// the value is never read back out of the keystore, so an empty box with
    /// a saved token is normal — and "paste it again" is advice the user
    /// cannot follow, because Atlassian shows a token exactly once.
    @State private var hasStoredToken = false

    init(appState: AppState, initialStep: Int, onClose: @escaping (Bool) -> Void) {
        self.appState = appState
        self.initialStep = initialStep
        self.onClose = onClose
        _step = State(initialValue: initialStep)
    }

    private var canContinue: Bool {
        step == 1 ? verified : !pickedId.isEmpty
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            header
            Divider()
            ScrollView {
                Group {
                    if step == 1 { credentialsStep } else { destinationStep }
                }
                .padding(.vertical, 4)
            }
            .frame(minHeight: 260)
            footer
        }
        .padding(24)
        .frame(minWidth: 560, minHeight: 460)
        .onAppear(perform: load)
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text("Connect Confluence").font(.system(size: 20, weight: .semibold))
            Text(step == 1
                 ? "Dimmy writes recap pages as you, with your own permissions. It never reads anything else."
                 : "Each recap becomes a new page in this space.")
                .font(.system(size: 12))
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    // ── Step 1 ──────────────────────────────────────────────────────

    private var credentialsStep: some View {
        VStack(alignment: .leading, spacing: 12) {
            labelled("Confluence site") {
                TextField("yourcompany.atlassian.net", text: $site)
                    .textFieldStyle(.roundedBorder)
                    .onChange(of: site) { _, _ in invalidate() }
            }
            labelled("Atlassian account email") {
                TextField("name@company.com", text: $email)
                    .textFieldStyle(.roundedBorder)
                    .onChange(of: email) { _, _ in invalidate() }
            }
            labelled(hasStoredToken ? "API token — already saved" : "API token") {
                SecureField(hasStoredToken
                            ? "Leave empty to keep it, or paste a new one to replace it"
                            : "Paste the token you just created", text: $token)
                    .textFieldStyle(.roundedBorder)
                    .onChange(of: token) { _, _ in invalidate() }
            }

            // Atlassian offers two kinds of token and the page does not say
            // which an app needs. Either works here; a scoped one needs these.
            Text("A plain API token works. If you create one with scopes, it needs "
                 + "read:space:confluence and write:page:confluence.")
                .font(.system(size: 11))
                .foregroundStyle(.tertiary)
                .fixedSize(horizontal: false, vertical: true)

            HStack(spacing: 10) {
                Button("Create an API token") {
                    if let url = URL(string: "https://id.atlassian.com/manage-profile/security/api-tokens") {
                        NSWorkspace.shared.open(url)
                    }
                }
                Button("Check") { check() }
                    .buttonStyle(.borderedProminent)
                    .disabled(busy || site.trimmingCharacters(in: .whitespaces).isEmpty
                              || email.trimmingCharacters(in: .whitespaces).isEmpty)
                if busy { ProgressView().controlSize(.small) }
            }

            if let s = status {
                MacNote(title: s.ok ? "Connected" : "Confluence said no",
                        message: s.text,
                        systemImage: s.ok ? "checkmark.circle.fill" : "exclamationmark.triangle.fill")
            }
        }
    }

    // ── Step 2 ──────────────────────────────────────────────────────

    private var destinationStep: some View {
        VStack(alignment: .leading, spacing: 12) {
            labelled("Space") {
                TextField("Type to filter, e.g. your name", text: $filter)
                    .textFieldStyle(.roundedBorder)
            }

            if loadingSpaces {
                HStack(spacing: 8) {
                    ProgressView().controlSize(.small)
                    Text("Loading spaces…").font(.system(size: 12)).foregroundStyle(.secondary)
                }
            } else if shown.isEmpty {
                Text(spaces.isEmpty ? "No spaces available." : "Nothing matches that filter.")
                    .font(.system(size: 12)).foregroundStyle(.secondary)
            } else {
                Picker("", selection: $pickedId) {
                    ForEach(shown) { space in
                        Text(space.label).tag(space.id)
                    }
                }
                .labelsHidden()
                .frame(maxWidth: .infinity)
            }

            Text("Recaps are sent when you click Send on a meeting. You can turn on "
                 + "automatic sending in Settings afterwards. Every page carries a "
                 + "visible note saying it was generated with AI.")
                .font(.system(size: 11))
                .foregroundStyle(.tertiary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    private var shown: [ConfluenceSpace] {
        let q = filter.trimmingCharacters(in: .whitespaces)
        guard !q.isEmpty else { return spaces }
        return spaces.filter {
            $0.name.localizedCaseInsensitiveContains(q)
                || $0.key.localizedCaseInsensitiveContains(q)
        }
    }

    // ── Footer ──────────────────────────────────────────────────────

    private var footer: some View {
        HStack {
            if step == 2 && initialStep == 1 {
                Button("Back") { step = 1 }
            }
            Spacer()
            Button("Cancel") { onClose(false) }
                .keyboardShortcut(.cancelAction)
            Button(step == 1 ? "Next" : "Done") {
                if step == 1 { step = 2; loadSpaces() } else { save() }
            }
            .buttonStyle(.borderedProminent)
            .disabled(!canContinue)
        }
    }

    // ── Actions ─────────────────────────────────────────────────────

    private func load() {
        hasStoredToken = DimmyCore.shared.confluenceHasToken
        // Into the state, not just the fields: entering at step 2 skips check(),
        // which is the only other place these are set.
        if let cfg = DimmyCore.shared.getConfig() {
            site = (cfg["confluence_site"] as? String) ?? ""
            email = (cfg["confluence_email"] as? String) ?? ""
            pickedId = (cfg["confluence_space_id"] as? String) ?? ""
        }
        if initialStep >= 2 && hasStoredToken {
            verified = true
            loadSpaces()
        }
    }

    /// Any edit invalidates the previous check: what is on screen is no longer
    /// what was proven to work.
    private func invalidate() {
        guard verified else { return }
        verified = false
        status = nil
    }

    private func check() {
        busy = true
        status = nil
        let s = site.trimmingCharacters(in: .whitespacesAndNewlines)
        let e = email.trimmingCharacters(in: .whitespacesAndNewlines)
        let t = token.trimmingCharacters(in: .whitespacesAndNewlines)
        DispatchQueue.global(qos: .userInitiated).async {
            let raw = DimmyCore.shared.confluenceTestConnection(site: s, email: e, token: t)
            let (ok, account, error) = decodeEnvelope(raw)
            DispatchQueue.main.async {
                busy = false
                verified = ok
                status = (ok,
                          ok ? (account.isEmpty ? "Credentials accepted." : "Connected as \(account).")
                             : (error.isEmpty ? "Could not reach Confluence." : error))
                // Save as soon as it is proven, so closing here and coming back
                // does not mean fetching the token again.
                if ok && !t.isEmpty {
                    DimmyCore.shared.confluenceSetToken(t)
                    hasStoredToken = true
                }
            }
        }
    }

    private func loadSpaces() {
        loadingSpaces = true
        let s = site
        let e = email
        DispatchQueue.global(qos: .userInitiated).async {
            let raw = DimmyCore.shared.confluenceSpaces(site: s, email: e)
            let list = decodeSpaces(raw)
            DispatchQueue.main.async {
                spaces = list
                loadingSpaces = false
                // Keep what was configured; otherwise the space the CORE
                // identified as ours. Never "the first personal one" — on a big
                // tenant that is a colleague.
                if !list.contains(where: { $0.id == pickedId }) {
                    pickedId = list.first(where: { $0.isMine })?.id ?? ""
                }
            }
        }
    }

    private func save() {
        guard let picked = spaces.first(where: { $0.id == pickedId }) else { return }
        var cfg = appState.toRustConfig()
        cfg["confluence_site"] = site
        cfg["confluence_email"] = email
        cfg["confluence_space_id"] = picked.id
        cfg["confluence_space_key"] = picked.key
        cfg["confluence_space_name"] = picked.name
        DimmyCore.shared.setConfig(cfg)
        onClose(true)
    }

    // ── Helpers ─────────────────────────────────────────────────────

    @ViewBuilder
    private func labelled<Content: View>(_ text: String,
                                         @ViewBuilder content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(text).font(.system(size: 11)).foregroundStyle(.secondary)
            content()
        }
    }

    private func decodeEnvelope(_ raw: String?) -> (Bool, String, String) {
        guard let raw, let data = raw.data(using: .utf8),
              let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        else { return (false, "", "Unexpected reply from the core.") }
        return ((obj["ok"] as? Bool) ?? false,
                (obj["account"] as? String) ?? "",
                (obj["error"] as? String) ?? "")
    }

    private func decodeSpaces(_ raw: String?) -> [ConfluenceSpace] {
        guard let raw, let data = raw.data(using: .utf8),
              let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              (obj["ok"] as? Bool) == true,
              let arr = obj["spaces"],
              let payload = try? JSONSerialization.data(withJSONObject: arr)
        else { return [] }
        return (try? JSONDecoder().decode([ConfluenceSpace].self, from: payload)) ?? []
    }
}

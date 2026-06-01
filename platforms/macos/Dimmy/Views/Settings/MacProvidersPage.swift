import SwiftUI

// MARK: - MacProvidersPage
//
// Mac port of the Windows Providers & keys page
// (platforms/windows/Dimmy.Windows/Views/SettingsWindow.Providers.cs,
// commit fcf03f67 on `feat/settings-redesign`).
//
// One DisclosureGroup per provider from `ProviderCatalog.all`. The
// header shows a logo tile, the provider name, a sub-status line and
// a status pill. The body shows:
//   - On-device: a "runs offline, no key" blurb.
//   - Keyable cloud vendors (Groq/OpenAI/Anthropic/Gemini/Deepgram/
//     OpenRouter/Fireworks/Together): a SecureField + Connect/Replace
//     button, a "Get a <Name> key ->" link to the vendor's console,
//     and a one-line getKeyHint.
//   - Custom: a "configure base URL on Voice/Output" note plus the
//     deep-link if any.
// Below the key area each card lists every model with STT / LLM /
// Recap capability badges, so the user sees what the provider can do
// here without having to flip back to the routing pickers.
//
// Backend is untouched: a connect click saves the same key to every
// capable scope via `dimmy_save_llm_provider_key`. "Connected" state
// is derived from the per-vendor keystore flags already on AppState.

struct MacProvidersPage: View {
    @ObservedObject var appState: AppState

    @State private var pasteBuffer: [String: String] = [:]
    @State private var revealedKeyField: String? = nil
    @State private var inFlight: Set<String> = []
    @State private var statusMessages: [String: (text: String, isError: Bool)] = [:]

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            MacNote(
                title: "One key per provider",
                message: "Paste a key once here and Dimmy uses it for every task the provider can do: speech to text, rewrite, recap. Keys are encrypted on disk with AES-256-GCM and never leave this Mac.",
                systemImage: "key.fill"
            )
            .padding(.bottom, 12)

            ForEach(ProviderCatalog.all, id: \.id) { provider in
                providerCard(provider)
                    .padding(.bottom, 8)
            }

            MacGroupFooter(text: "Keys live in ~/.config/dimmy/keys.enc. Each card opens its provider's console in your browser when you click \"Get a key\".")
        }
    }

    // MARK: - Card

    @ViewBuilder
    private func providerCard(_ p: ProviderInfo) -> some View {
        DisclosureGroup {
            cardBody(p)
                .padding(.top, 6)
        } label: {
            cardHeader(p)
        }
        .padding(EdgeInsets(top: 12, leading: 14, bottom: 12, trailing: 14))
        .background(
            RoundedRectangle(cornerRadius: MacTheme.tileCornerRadius, style: .continuous)
                .fill(Color(nsColor: .windowBackgroundColor).opacity(0.6))
        )
        .overlay(
            RoundedRectangle(cornerRadius: MacTheme.tileCornerRadius, style: .continuous)
                .stroke(Color.macStrokeHairline, lineWidth: 0.5)
        )
    }

    // MARK: - Header

    @ViewBuilder
    private func cardHeader(_ p: ProviderInfo) -> some View {
        let connected = isConnected(p)
        HStack(spacing: 12) {
            logoTile(p)
            VStack(alignment: .leading, spacing: 2) {
                Text(p.name)
                    .font(.system(size: 14, weight: .semibold))
                Text(subStatus(p, connected: connected))
                    .font(.system(size: 11))
                    .foregroundStyle(Color.macTextSecondary)
            }
            Spacer(minLength: 8)
            statusPill(connected: connected, hasKeyEntry: ProviderCatalog.isKeyableHere(p))
        }
    }

    /// Real brand mark on a white squircle tile. SwiftUI's `Image`
    /// renders the SVG from the asset catalog directly; if the
    /// imageset is somehow missing it will fall back to nothing on
    /// the host platform, so the white tile + the (already visible)
    /// provider name in the header are enough context. We avoid an
    /// NSImage(named:) probe because that path uses LaunchServices
    /// caches that do not always pick up newly-bundled imagesets.
    private func logoTile(_ p: ProviderInfo) -> some View {
        RoundedRectangle(cornerRadius: 6, style: .continuous)
            .fill(Color.white)
            .overlay(
                Image(p.id)
                    .renderingMode(.original)
                    .resizable()
                    .scaledToFit()
                    .padding(5)
            )
            .overlay(
                RoundedRectangle(cornerRadius: 6, style: .continuous)
                    .stroke(Color.macControlStroke, lineWidth: 0.5)
            )
            .frame(width: 32, height: 32)
    }

    private func statusPill(connected: Bool, hasKeyEntry: Bool) -> some View {
        let label: String
        let color: Color
        if !hasKeyEntry {
            label = "Configured elsewhere"
            color = Color.macTextSecondary
        } else if connected {
            label = "Connected"
            color = Color(red: 0.18, green: 0.70, blue: 0.48)
        } else {
            label = "Not connected"
            color = Color.macTextSecondary
        }
        return HStack(spacing: 6) {
            Circle()
                .fill(color)
                .frame(width: 6, height: 6)
            Text(label)
                .font(.system(size: 11, weight: .medium))
                .foregroundStyle(color)
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 3)
        .background(
            Capsule().fill(color.opacity(0.20))
        )
        .overlay(
            Capsule().stroke(color.opacity(0.50), lineWidth: 0.5)
        )
    }

    // MARK: - Body

    @ViewBuilder
    private func cardBody(_ p: ProviderInfo) -> some View {
        if p.id == "local" {
            Text("Runs entirely on this Mac, private, offline, and free. No API key needed. Pick the exact on-device model on the Voice input and Output pages.")
                .font(.system(size: 12))
                .foregroundStyle(Color.macTextSecondary)
                .fixedSize(horizontal: false, vertical: true)
            modelsList(p).padding(.top, 6)
        } else if ProviderCatalog.isKeyableHere(p) {
            keyRow(p)
            getKeyRow(p)
            modelsList(p).padding(.top, 4)
        } else {
            Text("Set a custom OpenAI-compatible endpoint and key on the Voice input page for speech, or the Output page for rewrite.")
                .font(.system(size: 12))
                .foregroundStyle(Color.macTextSecondary)
                .fixedSize(horizontal: false, vertical: true)
            getKeyRow(p)
            modelsList(p).padding(.top, 4)
        }
    }

    @ViewBuilder
    private func keyRow(_ p: ProviderInfo) -> some View {
        let connected = isConnected(p)
        let busy = inFlight.contains(p.id)
        let bufferKey = p.id
        HStack(spacing: 8) {
            SecureField(
                connected ? "Key saved. Paste a new one to replace." : "Paste your API key...",
                text: Binding(
                    get: { pasteBuffer[bufferKey] ?? "" },
                    set: { pasteBuffer[bufferKey] = $0 }
                )
            )
            .textFieldStyle(.roundedBorder)
            if connected {
                Button("Replace") { connect(p) }
                    .disabled(busy || (pasteBuffer[bufferKey] ?? "").isEmpty)
                    .buttonStyle(.bordered)
                    .controlSize(.small)
            } else {
                Button("Connect") { connect(p) }
                    .disabled(busy || (pasteBuffer[bufferKey] ?? "").isEmpty)
                    .buttonStyle(.borderedProminent)
                    .controlSize(.small)
            }
        }
        if connected {
            Button("Remove key") { remove(p) }
                .buttonStyle(.link)
                .font(.system(size: 11))
        }
        if let msg = statusMessages[p.id] {
            HStack(spacing: 4) {
                Image(systemName: msg.isError ? "exclamationmark.octagon.fill" : "checkmark.circle.fill")
                    .foregroundStyle(msg.isError ? .red : .green)
                Text(msg.text)
                    .font(.system(size: 11))
                    .foregroundStyle(Color.macTextSecondary)
            }
        }
    }

    @ViewBuilder
    private func getKeyRow(_ p: ProviderInfo) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            if let url = URL(string: p.consoleUrl), !p.consoleUrl.isEmpty {
                Link("Get a \(p.name) key \u{2192}", destination: url)
                    .font(.system(size: 12))
            }
            Text(p.getKeyHint)
                .font(.system(size: 11))
                .foregroundStyle(Color.macTextSecondary)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(.top, 2)
    }

    private func modelsList(_ p: ProviderInfo) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 6) {
                Text("MODELS")
                    .font(.system(size: 10, weight: .semibold))
                    .tracking(0.6)
                    .foregroundStyle(Color.macTextSecondary)
                Spacer()
            }
            .padding(.vertical, 6)

            VStack(spacing: 0) {
                ForEach(Array(p.models.enumerated()), id: \.offset) { idx, model in
                    if idx > 0 {
                        Divider().background(Color.macRowDivider).opacity(0.5)
                    }
                    HStack(spacing: 8) {
                        Text(model.name)
                            .font(.system(size: 12))
                        Spacer(minLength: 8)
                        if model.stt { capabilityBadge("STT", color: Color(red: 0.18, green: 0.70, blue: 0.48)) }
                        if model.llm { capabilityBadge("LLM", color: Color(red: 0.39, green: 0.45, blue: 1.00)) }
                        if model.recap { capabilityBadge("Recap", color: Color(red: 0.78, green: 0.40, blue: 0.90)) }
                    }
                    .padding(.vertical, 6)
                    .padding(.horizontal, 8)
                }
            }
            .background(
                RoundedRectangle(cornerRadius: 6, style: .continuous)
                    .fill(Color.primary.opacity(0.04))
            )
            .overlay(
                RoundedRectangle(cornerRadius: 6, style: .continuous)
                    .stroke(Color.macControlStroke, lineWidth: 0.5)
            )
        }
    }

    private func capabilityBadge(_ text: String, color: Color) -> some View {
        Text(text)
            .font(.system(size: 10, weight: .semibold))
            .padding(.horizontal, 6)
            .padding(.vertical, 1)
            .foregroundStyle(color)
            .background(
                // Dark theme needs more tint to surface the badge over
                // the dark tile; the earlier 0.12 fill was invisible.
                Capsule().fill(color.opacity(0.22))
            )
            .overlay(
                Capsule().stroke(color.opacity(0.55), lineWidth: 0.5)
            )
    }

    // MARK: - State helpers

    private func isConnected(_ p: ProviderInfo) -> Bool {
        if p.id == "local" { return true }
        return appState.hasAnyKeyForProvider(p.id)
    }

    private func subStatus(_ p: ProviderInfo, connected: Bool) -> String {
        if p.id == "local" {
            return "Runs offline. No key needed."
        }
        if !ProviderCatalog.isKeyableHere(p) {
            return "Set the endpoint and key on Voice input or Output."
        }
        let scopes = ProviderCatalog.keySaveScopes(p)
        let what = scopes.compactMap { scope -> String? in
            switch scope {
            case "stt": return "speech"
            case "llm": return "rewrite"
            case "recap": return nil  // implied by llm
            default: return nil
            }
        }.joined(separator: " and ")
        if connected {
            return "Connected. One key covers \(what.isEmpty ? "all tasks" : what)."
        }
        return "Add a key to enable \(what.isEmpty ? "this provider" : what)."
    }

    // MARK: - Actions

    private func connect(_ p: ProviderInfo) {
        let key = (pasteBuffer[p.id] ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        guard !key.isEmpty else { return }
        let scopes = ProviderCatalog.keySaveScopes(p)
        guard !scopes.isEmpty else {
            statusMessages[p.id] = ("Configure base URL on Voice input or Output.", true)
            return
        }
        inFlight.insert(p.id)
        statusMessages[p.id] = ("Saving key...", false)
        DispatchQueue.global(qos: .userInitiated).async {
            var ok = true
            for scope in scopes {
                let rc = scope.withCString { sp -> Int32 in
                    p.id.withCString { pp in
                        key.withCString { kp in
                            dimmy_save_llm_provider_key(sp, pp, kp)
                        }
                    }
                }
                if rc != 0 { ok = false }
            }
            DispatchQueue.main.async {
                inFlight.remove(p.id)
                if ok {
                    pasteBuffer[p.id] = ""
                    statusMessages[p.id] = ("Connected. Key saved for \(scopes.joined(separator: ", ")).", false)
                    if let cfg = DimmyCore.shared.getConfig() {
                        appState.loadFromRustConfig(cfg)
                    }
                } else {
                    statusMessages[p.id] = ("Could not save key. Check the log.", true)
                }
            }
        }
    }

    private func remove(_ p: ProviderInfo) {
        let scopes = ProviderCatalog.keySaveScopes(p)
        guard !scopes.isEmpty else { return }
        inFlight.insert(p.id)
        statusMessages[p.id] = ("Removing key...", false)
        DispatchQueue.global(qos: .userInitiated).async {
            for scope in scopes {
                _ = scope.withCString { sp -> Int32 in
                    p.id.withCString { pp in
                        "".withCString { kp in
                            dimmy_save_llm_provider_key(sp, pp, kp)
                        }
                    }
                }
            }
            DispatchQueue.main.async {
                inFlight.remove(p.id)
                statusMessages[p.id] = ("Key removed.", false)
                if let cfg = DimmyCore.shared.getConfig() {
                    appState.loadFromRustConfig(cfg)
                }
            }
        }
    }
}

// MARK: - Color hex helper

private extension Color {
    init?(hex: String) {
        let s = hex.trimmingCharacters(in: .whitespaces)
            .replacingOccurrences(of: "#", with: "")
        guard s.count == 6, let value = UInt32(s, radix: 16) else { return nil }
        let r = Double((value >> 16) & 0xFF) / 255.0
        let g = Double((value >> 8) & 0xFF) / 255.0
        let b = Double(value & 0xFF) / 255.0
        self = Color(red: r, green: g, blue: b)
    }
}

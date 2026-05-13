import SwiftUI

// Output — LLM enhancement style + provider + paste behaviour. Style
// chips replace the old Picker — coloured swatches make the dozen
// options scannable. Sub-mode (cloud / on-device) and provider routing
// follow the design's two-tier layout.

struct MacOutputPage: View {
    @ObservedObject var appState: AppState

    @State private var llmKeyInput: String = ""
    @State private var showLlmKeyField: Bool = false

    @State private var localLlmExists: Bool = false
    @State private var llmDownloadFailed: String? = nil

    /// Use the AppState-published flag (NOT a local @State) so the
    /// download progress survives the Settings page being recreated:
    /// SwiftUI tears the view down when you navigate to another page,
    /// which used to reset a local @State and re-show the Download
    /// button while the background download was still running. Two
    /// races later you'd have a corrupted `.part` file. Mirrors what
    /// `LLMModelSettingsView` already does for the OutputSettingsView.
    private var llmDownloadInFlight: Bool {
        appState.isDownloadingLlmModel
    }

    // MARK: - Provider matching + auth-method helpers
    //
    // Provider tag derived from a URL host. Used to decide whether the
    // STT and LLM endpoints accept the same vendor key (Groq STT +
    // Groq LLM → yes; Groq STT + OpenAI LLM → no). The `claude-code://`
    // synthetic URL is mapped to `anthropic` because the underlying
    // service is the same.
    private func providerTag(_ url: String) -> String {
        if url.hasPrefix("claude-code://") { return "anthropic" }
        if url.contains("groq.com") { return "groq" }
        if url.contains("openai.com") { return "openai" }
        if url.contains("openrouter.ai") { return "openrouter" }
        if url.contains("googleapis.com") { return "gemini" }
        if url.contains("anthropic.com") { return "anthropic" }
        if url.contains("fireworks.ai") { return "fireworks" }
        if url.contains("together.xyz") || url.contains("together.ai") { return "together" }
        if url.contains("deepgram.com") { return "deepgram" }
        return ""
    }

    private var sttProviderTag: String { providerTag(appState.apiUrl) }
    private var llmProviderTag: String { providerTag(appState.llmApiUrl) }

    /// Anthropic is the only provider with a dual-auth path
    /// (API key OR Claude Code subscription). We show the
    /// Authentication picker only for Anthropic.
    private var isAnthropicLlm: Bool {
        llmProviderTag == "anthropic"
    }

    /// The "Use same key as STT" toggle is meaningful only when STT
    /// and LLM share a vendor AND the LLM isn't using subscription
    /// auth (which has its own credential). For mismatched vendors we
    /// force a dedicated LLM key.
    private var sameKeyShouldShow: Bool {
        guard !sttProviderTag.isEmpty,
              sttProviderTag == llmProviderTag,
              appState.llmAuthMethod != "subscription" else { return false }
        return true
    }

    /// Human-readable label for the STT provider — used in the
    /// "Use same \(provider) key" description so the user knows what
    /// the toggle actually reuses.
    private var sttProviderLabel: String {
        switch sttProviderTag {
        case "groq": return "Groq"
        case "openai": return "OpenAI"
        case "openrouter": return "OpenRouter"
        case "gemini": return "Gemini"
        case "anthropic": return "Anthropic"
        case "fireworks": return "Fireworks"
        case "together": return "Together"
        case "deepgram": return "Deepgram"
        default: return "STT"
        }
    }

    private var authMethodDescription: String {
        appState.llmAuthMethod == "subscription"
            ? "Claude Pro / Team / Max — routed through the local `claude` CLI. No API key needed."
            : "Direct Anthropic API key — pay-as-you-go billing."
    }

    /// Side-effects of flipping the auth-method radio: if the user
    /// moves to API key while the URL is the magic `claude-code://`
    /// (which Rust still routes through the subscription CLI), swap
    /// it for the real Anthropic endpoint so the API-key path is
    /// actually used. Going the other direction we keep the URL —
    /// Rust only checks `auth_method == "subscription"`.
    private func normalizeLlmUrlForAuth(_ method: String) {
        if method == "api_key" && appState.llmApiUrl.hasPrefix("claude-code://") {
            appState.llmApiUrl = "https://api.anthropic.com/v1/messages"
            if appState.llmApiModel.isEmpty {
                appState.llmApiModel = "claude-opus-4-7"
            }
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            rewriteStyleGroup
            llmModeGroup
            recapModelGroup
            pasteboardGroup

            if appState.showAdvanced {
                advancedLlmGroup
            }
        }
    }

    /// Meeting / long-dictation recap model picker. Always visible so a
    /// fresh user can pick their preferred recap model without flipping
    /// the Advanced toggle. Mirrors the picker in Settings → Advanced;
    /// both write to the same `recap_model_override` config field. The
    /// `local:` / `cloud:` prefixing is handled by `RecapModelOption`.
    private var recapModelGroup: some View {
        Group {
            MacGroupLabel(text: "Meeting recap model")
            MacTile {
                MacRow(
                    "Recap model",
                    description: "Used for the meeting recap pipeline and the long-dictation auto-recap. Auto matches your dictation provider. Pick a local Gemma to run the recap offline.",
                    showsDivider: false
                ) {
                    Picker("", selection: Binding<String>(
                        get: { appState.recapModelOverride },
                        set: { newValue in
                            appState.recapModelOverride = newValue
                            persistConfig()
                        }
                    )) {
                        ForEach(RecapModelOption.curated) { opt in
                            Label(opt.label, systemImage: opt.iconName)
                                .tag(opt.id)
                        }
                        if !appState.recapModelOverride.isEmpty,
                           !RecapModelOption.curated.contains(where: { $0.id == appState.recapModelOverride }) {
                            Divider()
                            Label("Custom — \(appState.recapModelOverride)",
                                  systemImage: "wrench.adjustable")
                                .tag(appState.recapModelOverride)
                        }
                    }
                    .labelsHidden()
                    .pickerStyle(.menu)
                    .frame(minWidth: 280)
                }
            }
            MacGroupFooter(text: "Cloud entries use your configured LLM API key. Local entries use the matching Gemma `.gguf` from Settings → Voice (download required).")
        }
    }

    // MARK: Rewrite style (chip picker)

    private var rewriteStyleGroup: some View {
        Group {
            MacGroupLabel(text: "Rewrite style")
            MacTile {
                VStack(alignment: .leading, spacing: 12) {
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Style").font(.system(size: 13))
                        Text("After transcribing, Dimmy rewrites your text before pasting.")
                            .font(.system(size: 11))
                            .foregroundStyle(Color.macTextSecondary)
                    }
                    chipFlow
                }
                .padding(EdgeInsets(top: 12, leading: 14, bottom: 12, trailing: 14))
            }
        }
    }

    /// Flexbox-style wrap of style chips. SwiftUI doesn't have native
    /// flex-wrap, so we lay out manually using `LazyVGrid` with adaptive
    /// columns sized to the chip width.
    private var chipFlow: some View {
        LazyVGrid(
            columns: [GridItem(.adaptive(minimum: 110), spacing: 6)],
            alignment: .leading,
            spacing: 6
        ) {
            ForEach(MacLlmStyles, id: \.key) { entry in
                MacChip(
                    label: entry.label,
                    color: MacStyleColor.color(for: entry.key),
                    selected: appState.llmStyle == entry.key
                ) {
                    appState.llmStyle = entry.key
                    appState.llmEnabled = entry.key != "off"
                    persistConfig()
                }
            }
        }
    }

    // MARK: LLM mode + provider

    private var llmModeGroup: some View {
        Group {
            MacGroupLabel(text: "LLM provider")
            MacTile {
                MacRow("Mode", description: "Where the rewrite runs") {
                    Picker("", selection: Binding(
                        get: { appState.llmMode },
                        set: { newValue in
                            appState.llmMode = newValue
                            persistConfig()
                        }
                    )) {
                        Text("On device").tag("local")
                        Text("Cloud").tag("cloud")
                    }
                    .pickerStyle(.segmented)
                    .labelsHidden()
                    .frame(width: 180)
                }

                if appState.llmMode == "cloud" {
                    MacRow("Provider") {
                        Picker("", selection: llmPresetBinding) {
                            ForEach(LlmPreset.presets) { preset in
                                Label {
                                    Text(preset.displayName)
                                } icon: {
                                    if preset.iconAssetName.isEmpty {
                                        Image(systemName: "gear")
                                    } else {
                                        Image(preset.iconAssetName)
                                            .renderingMode(.original)
                                            .resizable()
                                            .frame(width: 12, height: 12)
                                    }
                                }
                                .tag(preset.id)
                            }
                        }
                        .labelsHidden()
                        .frame(width: 320)
                    }

                    // Anthropic-only: Authentication picker. The user
                    // chooses between an Anthropic API key (pay-as-
                    // you-go) and a Claude Pro / Team / Max
                    // subscription routed via the local `claude` CLI.
                    // Mirrors the Win redesign — auth is orthogonal to
                    // provider+model. Shown for any Anthropic URL OR
                    // for the legacy `claude-code://` synthetic URL.
                    if isAnthropicLlm {
                        MacRow(
                            "Authentication",
                            description: authMethodDescription,
                            showsDivider: appState.llmAuthMethod == "subscription" || sameKeyShouldShow
                        ) {
                            Picker("", selection: Binding(
                                get: { appState.llmAuthMethod == "subscription" ? "subscription" : "api_key" },
                                set: { newValue in
                                    appState.llmAuthMethod = newValue
                                    normalizeLlmUrlForAuth(newValue)
                                    persistConfig()
                                }
                            )) {
                                Text("API key").tag("api_key")
                                Text("Subscription").tag("subscription")
                            }
                            .pickerStyle(.segmented)
                            .labelsHidden()
                            .frame(width: 220)
                        }
                    }

                    // Subscription branch: show the Claude Code status
                    // card (binary detection, sign-in via browser, ping
                    // test). When subscription is selected the API-key
                    // entry + same-key toggle disappear — the CLI owns
                    // the credential.
                    if appState.llmAuthMethod == "subscription" || appState.llmApiUrl.hasPrefix("claude-code://") {
                        MacClaudeCodeCard(appState: appState)
                    } else {
                        // API-key branch. The "Use same key as STT"
                        // toggle is only meaningful when STT and LLM
                        // share a vendor (and the key endpoint thus
                        // accepts the same token). Different vendors →
                        // we hide the toggle and force a dedicated key.
                        if sameKeyShouldShow {
                            MacRow(
                                "Use same key as STT",
                                description: "Reuse the \(sttProviderLabel) key for the LLM provider — both endpoints accept the same token.",
                                showsDivider: !appState.llmUseSameKey
                            ) {
                                Toggle("", isOn: Binding(
                                    get: { appState.llmUseSameKey },
                                    set: { newValue in
                                        appState.llmUseSameKey = newValue
                                        persistConfig()
                                    }
                                ))
                                .toggleStyle(.switch)
                                .labelsHidden()
                            }
                        }

                        if !sameKeyShouldShow || !appState.llmUseSameKey {
                            MacRow(
                                "LLM API key",
                                description: sameKeyShouldShow
                                    ? "Encrypted locally. Used when not sharing with STT."
                                    : "Encrypted locally. STT and LLM use different providers, so a dedicated key is required.",
                                showsDivider: showLlmKeyField
                            ) {
                                if appState.hasLlmKey {
                                    HStack(spacing: 4) {
                                        Image(systemName: "checkmark.circle.fill")
                                            .foregroundStyle(.green)
                                        Text("Saved")
                                            .font(.system(size: 12, weight: .medium))
                                            .foregroundStyle(.green)
                                    }
                                }
                                Button(appState.hasLlmKey ? (showLlmKeyField ? "Cancel" : "Replace…")
                                                          : (showLlmKeyField ? "Cancel" : "Add key…")) {
                                    showLlmKeyField.toggle()
                                    if !showLlmKeyField { llmKeyInput = "" }
                                }
                                .controlSize(.small)
                            }

                            if showLlmKeyField {
                                llmKeyEntryRow
                            }
                        }
                    }
                } else {
                    MacRow(
                        "Local model",
                        description: "Runs entirely on your device",
                        showsDivider: !localLlmExists || llmDownloadInFlight
                    ) {
                        Picker("", selection: Binding(
                            get: { appState.localLlmModel },
                            set: { newValue in
                                appState.localLlmModel = newValue
                                persistConfig()
                                refreshLocalLlmStatus()
                            }
                        )) {
                            // Gemma 4 family — same catalogue as
                            // core/src/local_llm.rs AVAILABLE_LLM_MODELS.
                            // Order: small→large so the dropdown lines up
                            // with VRAM requirements top-to-bottom.
                            Text("Gemma 4 E2B Q4 · 3.1 GB · fast").tag("gemma-4-E2B-it-Q4_K_M.gguf")
                            Text("Gemma 4 E2B Q5 · 3.7 GB").tag("gemma-4-E2B-it-Q5_K_M.gguf")
                            Text("Gemma 4 E4B Q3 · 4.1 GB").tag("gemma-4-E4B-it-Q3_K_M.gguf")
                            Text("Gemma 4 E4B Q4 · 5.0 GB · recommended").tag("gemma-4-E4B-it-Q4_K_M.gguf")
                            Text("Gemma 4 E4B Q8 · 8.2 GB · max quality").tag("gemma-4-E4B-it-Q8_0.gguf")
                            // Phi-4 Mini — multilingual fallback
                            Text("Phi-4 Mini Q4 · 2.5 GB").tag("phi-4-mini-instruct-q4_k_m.gguf")
                        }
                        .labelsHidden()
                        .frame(width: 280)
                    }

                    if llmDownloadInFlight {
                        MacRow("Downloading \(appState.localLlmModel)…", showsDivider: false) {
                            HStack(spacing: 8) {
                                ProgressView(value: appState.llmModelDownloadProgress)
                                    .frame(width: 160)
                                Text(String(format: "%.0f%%", appState.llmModelDownloadProgress * 100))
                                    .font(.system(size: 12, design: .monospaced))
                                    .foregroundStyle(Color.macTextSecondary)
                                    .frame(width: 40, alignment: .trailing)
                            }
                        }
                    } else if !localLlmExists {
                        MacRow(
                            "Download",
                            description: llmDownloadFailed ?? "This model isn't on disk yet.",
                            showsDivider: false
                        ) {
                            Button("Download model") { startLlmDownload() }
                                .buttonStyle(.borderedProminent)
                                .controlSize(.small)
                        }
                    }
                }
            }
        }
        .onAppear { refreshLocalLlmStatus() }
    }

    private func refreshLocalLlmStatus() {
        guard DimmyCore.shared.isInitialized else {
            localLlmExists = false
            return
        }
        // Check via the listLLMModels payload — each entry has `downloaded`.
        if let arr = DimmyCore.shared.listLLMModels() {
            let me = arr.first(where: {
                ($0["filename"] as? String) == appState.localLlmModel
            })
            localLlmExists = (me?["downloaded"] as? Bool) ?? false
        } else {
            localLlmExists = false
        }
        llmDownloadFailed = nil
    }

    private func startLlmDownload() {
        // Belt-and-braces guard: the AppState flag survives view tear-
        // down, so a re-entered MacOutputPage with an in-flight
        // download still sees `true` and the button is hidden. Without
        // this guard a fast double-click could still slip through
        // before the @Published value propagates.
        guard !appState.isDownloadingLlmModel, DimmyCore.shared.isInitialized else { return }
        let target = appState.localLlmModel
        appState.isDownloadingLlmModel = true
        llmDownloadFailed = nil
        appState.llmModelDownloadProgress = 0
        DispatchQueue.global(qos: .userInitiated).async {
            let ok = DimmyCore.shared.downloadLLMModel(target)
            DispatchQueue.main.async {
                appState.isDownloadingLlmModel = false
                if ok {
                    refreshLocalLlmStatus()
                } else {
                    llmDownloadFailed = "Download failed. Check your connection and try again."
                }
            }
        }
    }

    /// Inline SecureField revealed under "LLM API key" when the user opts in.
    /// Writes `llm_api_key` (separate from STT `api_key`) and re-reads config
    /// to refresh `hasLlmKey`.
    private var llmKeyEntryRow: some View {
        MacRow("Paste key", showsDivider: false) {
            SecureField("sk-…", text: $llmKeyInput)
                .textFieldStyle(.roundedBorder)
                .frame(width: 240)
                .onSubmit { saveLlmKey() }
            Button("Save") { saveLlmKey() }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .disabled(llmKeyInput.isEmpty)
        }
    }

    private func saveLlmKey() {
        guard !llmKeyInput.isEmpty else { return }
        var config = appState.toRustConfig()
        config["llm_api_key"] = llmKeyInput
        DimmyCore.shared.setConfig(config)
        llmKeyInput = ""
        showLlmKeyField = false
        if let cfg = DimmyCore.shared.getConfig() {
            appState.loadFromRustConfig(cfg)
        }
    }

    // MARK: Pasteboard

    private var pasteboardGroup: some View {
        Group {
            MacGroupLabel(text: "Pasteboard")
            MacTile {
                MacRow(
                    "Keep in clipboard history",
                    description: "Leave the transcription on the clipboard after auto-paste so you can paste it again later.",
                    showsDivider: false
                ) {
                    Toggle("", isOn: Binding(
                        get: { appState.keepInClipboard },
                        set: { newValue in
                            appState.keepInClipboard = newValue
                            persistConfig()
                        }
                    ))
                    .toggleStyle(.switch)
                    .labelsHidden()
                }
            }
        }
    }

    // MARK: Advanced — tone, translate, custom prompt

    private var advancedLlmGroup: some View {
        Group {
            MacGroupLabel(text: "Advanced LLM")
            MacTile {
                MacRow("Tone", description: "Adjusts how formal or casual the rewrite sounds") {
                    Picker("", selection: Binding(
                        get: { appState.llmTone },
                        set: { newValue in
                            appState.llmTone = newValue
                            persistConfig()
                        }
                    )) {
                        ForEach(LlmTone.allCases) { tone in
                            Text(tone.displayName).tag(tone.rawValue)
                        }
                    }
                    .labelsHidden()
                    .frame(width: 160)
                }

                MacRow(
                    "Translate output to",
                    description: "Translate the LLM rewrite to this language. The pill's scroll wheel changes the same setting."
                ) {
                    Picker("", selection: Binding(
                        get: { appState.llmTranslateTo },
                        set: { newValue in
                            appState.llmTranslateTo = newValue
                            persistConfig()
                        }
                    )) {
                        Text("No translation").tag("")
                        Text("Italiano").tag("it")
                        Text("English").tag("en")
                        Text("Español").tag("es")
                        Text("Français").tag("fr")
                        Text("Deutsch").tag("de")
                        Text("Português").tag("pt")
                    }
                    .labelsHidden()
                    .frame(width: 180)
                }

                VStack(alignment: .leading, spacing: 8) {
                    Text("Custom prompt")
                        .font(.system(size: 13))
                    Text("Free-form instruction prepended to the LLM prompt for the Custom style.")
                        .font(.system(size: 11))
                        .foregroundStyle(Color.macTextSecondary)
                    TextEditor(text: Binding(
                        get: { appState.llmCustomPrompt },
                        set: { newValue in
                            appState.llmCustomPrompt = newValue
                            persistConfig()
                        }
                    ))
                    .font(.system(size: 12, design: .monospaced))
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
                }
                .padding(EdgeInsets(top: 12, leading: 14, bottom: 12, trailing: 14))
            }
        }
    }

    // MARK: Bindings + persistence

    private var llmPresetBinding: Binding<String> {
        Binding(
            get: {
                LlmPreset.find(url: appState.llmApiUrl, model: appState.llmApiModel)?.id
                    ?? "groq-llama70b"
            },
            set: { newValue in
                if let preset = LlmPreset.presets.first(where: { $0.id == newValue }) {
                    appState.llmApiUrl = preset.apiUrl
                    appState.llmApiModel = preset.model
                    persistConfig()
                }
            }
        )
    }

    private func persistConfig() {
        DimmyCore.shared.setConfig(appState.toRustConfig())
    }
}

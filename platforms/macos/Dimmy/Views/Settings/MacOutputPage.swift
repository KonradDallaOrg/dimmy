import SwiftUI

// Output — LLM enhancement style + provider + paste behaviour. Style
// chips replace the old Picker — coloured swatches make the dozen
// options scannable. Sub-mode (cloud / on-device) and provider routing
// follow the design's two-tier layout.

struct MacOutputPage: View {
    @ObservedObject var appState: AppState

    @State private var llmKeyInput: String = ""
    @State private var showLlmKeyField: Bool = false

    // Recap-vendor key save — separate field from the dictation
    // llmKeyInput. Routes to `dimmy_save_llm_provider_key` under
    // KeyringScope::Recap(<recap_provider>), leaving the dictation
    // llm_api_url + dictation key untouched. Used when the user
    // picks a recap provider that needs its own key (either cross-
    // vendor or same-vendor with the toggle turned off). The
    // `showRecapKeyField` flag mirrors `showLlmKeyField` so the
    // reveal-on-button pattern is identical across STT / LLM / Recap.
    @State private var recapKeyInput: String = ""
    @State private var showRecapKeyField: Bool = false

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
    // Pure logic lives in `ProviderTagging` (Utilities/) so the same
    // rules can be exercised from `SelfTests` without standing up a
    // SwiftUI surface. The view-level computed properties below are
    // thin pass-throughs that read from `appState`.

    private var sttProviderTag: String { ProviderTagging.providerTag(forUrl: appState.apiUrl) }
    private var llmProviderTag: String { ProviderTagging.providerTag(forUrl: appState.llmApiUrl) }

    /// Anthropic is the only provider with a dual-auth path
    /// (API key OR Claude Code subscription). We show the
    /// Authentication picker only for Anthropic.
    private var isAnthropicLlm: Bool {
        llmProviderTag == "anthropic"
    }

    /// The "Use same key as STT" toggle is meaningful only when STT
    /// and LLM share a vendor AND the LLM isn't using subscription
    /// auth. Logic lives in `ProviderTagging.sameKeyShouldShow` so it
    /// can be unit-tested.
    private var sameKeyShouldShow: Bool {
        ProviderTagging.sameKeyShouldShow(
            sttUrl: appState.apiUrl,
            llmUrl: appState.llmApiUrl,
            llmAuthMethod: appState.llmAuthMethod
        )
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

    /// Recap effective provider tag — derived purely from the chosen
    /// recap model id. claude-* → anthropic, gpt-/o3 → openai,
    /// gemini-* → gemini, "" (Auto) or unknown → "" (falls back to
    /// the dictation provider in the dispatcher).
    ///
    /// Drives auth gating (subscription only for Anthropic) and the
    /// conditional "Recap API key" field that appears when the
    /// model's vendor differs from the dictation LLM.
    private var recapProviderTag: String {
        ProviderTagging.providerTag(forRecapModel: appState.recapModelOverride)
    }

    /// True when the user has picked a CLOUD recap model whose vendor
    /// differs from the LLM provider. In that case the current Rust
    /// dispatcher still reuses `llm_api_url + llm_api_key`, which fails
    /// HTTP 400. We surface this clearly so the user picks either a
    /// matching cloud model, a local one, or (for Anthropic) the
    /// subscription path.
    private var recapVendorMismatch: Bool {
        let recap = recapProviderTag
        let llm = llmProviderTag
        guard !recap.isEmpty, recap != "local", recap != llm else { return false }
        return true
    }

    /// Should we offer the "Use Anthropic subscription for recap"
    /// toggle? Only when the recap model is Anthropic AND the user
    /// has actually connected Claude Code. Otherwise the toggle is
    /// either irrelevant (non-Anthropic recap) or non-actionable
    /// (no subscription connection yet).
    private var recapSubscriptionAvailable: Bool {
        recapProviderTag == "anthropic" && appState.claudeCodeReady
    }

    /// True when a Recap-scope key for the chosen vendor is already
    /// saved in the keystore. Drives the green ✓ next to the recap
    /// key field. Mirrors Win `HasRecapKey` (SettingsWindow.xaml.cs:3771)
    /// which reads `has_<vendor>_recap_key` — the dedicated Recap
    /// keystore scope, separate from the LLM-scope and STT-scope keys.
    private var recapKeyAlreadySaved: Bool {
        let vendor = recapProviderTag
        guard !vendor.isEmpty else { return false }
        return appState.recapKeyByVendor[vendor] ?? false
    }

    /// True when an upstream key exists for the recap vendor in either
    /// the LLM-scope (`has_<vendor>_llm_key`) or the STT-scope
    /// (`has_<vendor>_key`) keystore. When true, we offer the
    /// "Use same key" toggle so the user can reuse what they've already
    /// saved instead of pasting another key into the Recap scope.
    /// Mirrors Win lines 3744-3750.
    private var recapUpstreamKeyAvailable: Bool {
        let vendor = recapProviderTag
        guard !vendor.isEmpty else { return false }
        if appState.llmKeyByVendor[vendor] == true { return true }
        switch vendor {
        case "groq": return appState.hasGroqKey
        case "openai": return appState.hasOpenaiKey
        case "gemini": return appState.hasGeminiKey
        case "fireworks": return appState.hasFireworksKey
        case "together": return appState.hasTogetherKey
        case "custom": return appState.hasCustomKey
        default: return false
        }
    }

    /// True when the recap call is effectively routed through the
    /// Anthropic subscription (Claude Code CLI). When active, the
    /// Recap key UI is entirely hidden — the CLI handles its own auth.
    /// Win parity: SettingsWindow.xaml.cs:3686.
    /// Recap auth is INDEPENDENT of dictation since 2026-05-16 — the
    /// previous "empty == inherit from llm_auth_method" loop caused
    /// the dictation=subscription + recap=Gemini bug where the Gemini
    /// model was routed to the `claude` CLI ("HTTP 1" error).
    private var recapSubscriptionActive: Bool {
        guard recapSubscriptionAvailable else { return false }
        return appState.recapAuthMethod == "subscription"
    }

    /// True when the chosen recap model has a concrete cloud vendor
    /// the dispatcher can route to (not Auto, not Local). Below this
    /// gate the recap call inherits the dictation provider so no
    /// dedicated key UI is needed.
    private var recapVendorKnown: Bool {
        let r = recapProviderTag
        return !r.isEmpty && r != "local"
    }

    /// Should we surface the "Use same key as <vendor>" toggle for the
    /// recap section? Visible when:
    ///   - recap vendor is known (cloud, not Auto/Local)
    ///   - subscription path is NOT active (then no key needed at all)
    ///   - an upstream key for this vendor exists in LLM scope or
    ///     STT scope (otherwise the toggle has nothing to reuse — we
    ///     show the key field directly instead)
    ///
    /// Note the toggle ALSO appears when the recap vendor matches the
    /// LLM vendor — user reported they couldn't enter a dedicated
    /// recap key when recap was same-vendor as LLM. Win parity: the
    /// matrix in `UpdateRecapKeyCardVisibility` is gated on "upstream
    /// key exists", not on "vendor mismatch".
    private var recapUseSameKeyToggleShouldShow: Bool {
        recapVendorKnown
            && !recapSubscriptionActive
            && recapUpstreamKeyAvailable
    }

    /// Should we show the Recap API key entry row? Visible when:
    ///   - recap vendor is known + subscription inactive
    ///   - AND either no upstream to reuse, or user toggled "same key" OFF
    /// Mirrors Win behaviour for both same-vendor and cross-vendor cases.
    private var recapKeyFieldShouldShow: Bool {
        guard recapVendorKnown, !recapSubscriptionActive else { return false }
        if !recapUpstreamKeyAvailable { return true }
        return !appState.recapUseSameKey
    }

    /// Persist the recap-vendor's key via the dedicated FFI. Vendor
    /// is DERIVED from the chosen recap model (no separate provider
    /// control). Routes to `KeyringScope::Recap(<vendor>)` without
    /// touching the dictation llm_api_url. Empty key clears the entry.
    ///
    /// We deliberately DON'T guard on `vendor != llmProviderTag` —
    /// the user is allowed to store a separate Recap-scope key even
    /// when same-vendor as the LLM (e.g. different sub-account for
    /// billing/quota isolation). When same-vendor + toggle ON, the
    /// dispatcher reuses the LLM key; with this entry the user can
    /// override by toggling OFF.
    private func saveRecapProviderKey() {
        let vendor = recapProviderTag
        guard !vendor.isEmpty else { return }
        // scope="recap" → dedicated Recap(<vendor>) keystore entry,
        // separate from the LLM scope for the same vendor.
        let rc = "recap".withCString { sp -> Int32 in
            vendor.withCString { vp in
                recapKeyInput.withCString { kp in
                    dimmy_save_llm_provider_key(sp, vp, kp)
                }
            }
        }
        if rc == 0 {
            recapKeyInput = ""
            // No explicit refresh needed — `recapKeyAlreadySaved`
            // reads `dimmy_get_config_json` on demand each render.
        } else {
            NSLog("[Auth] dimmy_save_llm_provider_key rc=\(rc) provider=\(vendor)")
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
                            Label {
                                Text(opt.label)
                            } icon: {
                                // Real vendor logo when we have one, SF
                                // Symbol fallback for Auto / Local /
                                // Custom rows (no Anthropic/Gemini/etc.
                                // brand to render).
                                if opt.assetName.isEmpty {
                                    Image(systemName: opt.iconName)
                                } else {
                                    Image(opt.assetName)
                                        .renderingMode(.original)
                                        .resizable()
                                        .scaledToFit()
                                        .frame(width: 18, height: 18)
                                }
                            }
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

                // Subscription toggle for the recap call site —
                // only appears when (a) recap model is Anthropic AND
                // (b) Claude Code integration is actually connected.
                // For any other recap provider the toggle is hidden
                // entirely (user's rule: "se uso modello diverso da
                // antrop. non devo poter selezionare usa subs
                // atropic sotto"). Writes `recap_auth_method` so the
                // recap call routes through the local `claude` CLI
                // even when dictation stays on API key.
                if recapSubscriptionAvailable {
                    MacRow(
                        "Use Anthropic subscription for recap",
                        description: "Routes the recap LLM call through Claude Code (Pro / Team / Max). Dictation rewrite keeps its own auth method.",
                        showsDivider: recapUseSameKeyToggleShouldShow || recapKeyFieldShouldShow
                    ) {
                        Toggle("", isOn: Binding(
                            // Recap auth is independent of dictation
                            // since 2026-05-16. GET reads the literal
                            // recap_auth_method only — no inheritance.
                            // Empty == api_key (default).
                            get: {
                                appState.recapAuthMethod == "subscription"
                            },
                            // SET writes an EXPLICIT choice. We still
                            // write "api_key" for OFF (not "") so the
                            // field is unambiguous in config.json —
                            // an empty string is also accepted by the
                            // Rust core as api_key, but explicit values
                            // are easier to grep in logs.
                            set: { newValue in
                                appState.recapAuthMethod = newValue ? "subscription" : "api_key"
                                persistConfig()
                            }
                        ))
                        .toggleStyle(.switch)
                        .labelsHidden()
                    }
                }

                // Key reuse toggle for the recap call. Appears whenever
                // an upstream key for the recap vendor is already in
                // the keystore (LLM-scope or STT-scope) — same-vendor
                // case too (LLM = Anthropic + Recap = Anthropic). Default
                // ON; user can flip it OFF to force a dedicated
                // Recap-scope key below. Win parity:
                // SettingsWindow.xaml.cs:3744-3750.
                if recapUseSameKeyToggleShouldShow {
                    MacRow(
                        "Use same key as \(recapProviderTag.capitalized)",
                        description: "Reuse the \(recapProviderTag.capitalized) key you already saved for STT/LLM — no need to paste a separate Recap key.",
                        showsDivider: recapKeyFieldShouldShow || !appState.recapUseSameKey
                    ) {
                        Toggle("", isOn: Binding(
                            get: { appState.recapUseSameKey },
                            set: { newValue in
                                appState.recapUseSameKey = newValue
                                persistConfig()
                            }
                        ))
                        .toggleStyle(.switch)
                        .labelsHidden()
                    }
                }

                // Dedicated Recap-scope API key. Visible whenever the
                // recap vendor is known AND (no upstream key exists,
                // OR the user toggled "use same key" OFF above). The
                // user CAN store a separate recap key even when same-
                // vendor as the LLM — useful for splitting billing or
                // routing recap through a different sub-account. Writes
                // via `dimmy_save_llm_provider_key("recap", vendor, key)`.
                // Win parity: SettingsWindow.xaml.cs:3795-3823.
                if recapKeyFieldShouldShow {
                    MacRow(
                        "Recap API key",
                        description: !recapUpstreamKeyAvailable
                            ? "Encrypted locally. No upstream \(recapProviderTag.capitalized) key found for STT/LLM, so the recap call needs a dedicated one."
                            : "Encrypted locally. Used because you turned off key sharing above.",
                        showsDivider: showRecapKeyField
                    ) {
                        if recapKeyAlreadySaved {
                            HStack(spacing: 4) {
                                Image(systemName: "checkmark.circle.fill")
                                    .foregroundStyle(.green)
                                Text("Saved")
                                    .font(.system(size: 12, weight: .medium))
                                    .foregroundStyle(.green)
                            }
                        }
                        Button(recapKeyAlreadySaved
                               ? (showRecapKeyField ? "Cancel" : "Replace…")
                               : (showRecapKeyField ? "Cancel" : "Add key…")) {
                            showRecapKeyField.toggle()
                            if !showRecapKeyField { recapKeyInput = "" }
                        }
                        .controlSize(.small)
                    }

                    if showRecapKeyField {
                        recapKeyEntryRow
                    }
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
                                        // 18x18 + scaledToFit: 12 was too
                                        // tight for the Gemini "sparkle"
                                        // star (thin outline → looked
                                        // like a single dot). Same size
                                        // works for the denser logos
                                        // (Anthropic A, Groq, OpenAI).
                                        Image(preset.iconAssetName)
                                            .renderingMode(.original)
                                            .resizable()
                                            .scaledToFit()
                                            .frame(width: 18, height: 18)
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
                    //
                    // Subscription is ONLY offered when the Anthropic
                    // integration is actually connected (binary
                    // detected + Keychain / credentials.json present).
                    // No connection → segmented disappears and we show
                    // a one-line hint pointing to Integrations.
                    //
                    // Already-on-subscription users keep seeing the
                    // picker even if the connection probe goes red
                    // (e.g. user re-launched without logging back in)
                    // so they can switch back to API key explicitly.
                    if isAnthropicLlm {
                        let canSwitchToSubscription =
                            appState.claudeCodeReady || appState.llmAuthMethod == "subscription"
                        if canSwitchToSubscription {
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
                        } else {
                            MacRow(
                                "Subscription auth not available",
                                description: "Connect Claude Code in Settings → Integrations to use your Anthropic Pro / Team / Max subscription. Otherwise pay-as-you-go API key below.",
                                showsDivider: true
                            ) {
                                EmptyView()
                            }
                        }
                    }

                    // Subscription branch: the actual sign-in /
                    // connection card lives in Settings → Integrations
                    // (Anthropic is a connection like Notion, not a
                    // pure provider config). Here we just show a tiny
                    // "Manage in Integrations" pointer so users see
                    // the auth choice took effect.
                    if appState.llmAuthMethod == "subscription" || appState.llmApiUrl.hasPrefix("claude-code://") {
                        MacRow(
                            "Subscription connection",
                            description: appState.claudeCodeReady
                                ? "✓ Connected. Sign-in / disconnect lives in Settings → Integrations. The token is read on every LLM call via the local `claude` CLI."
                                : "⚠ Subscription selected but Claude Code isn't connected — calls will fail. Open Settings → Integrations to sign in.",
                            showsDivider: false
                        ) {
                            EmptyView()
                        }
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
        .onAppear {
            refreshLocalLlmStatus()
            appState.refreshClaudeCodeStatus()
        }
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
        // includeLlm:true — user is wiring up the LLM key and the
        // RECAP section + LLM URL fields must travel with it, else
        // the Rust core sees only `llm_api_key` and the URL/model
        // chosen in this same UI step never makes it to disk.
        var config = appState.toRustConfig(includeLlm: true, includeRecap: true)
        config["llm_api_key"] = llmKeyInput
        DimmyCore.shared.setConfig(config)
        llmKeyInput = ""
        showLlmKeyField = false
        if let cfg = DimmyCore.shared.getConfig() {
            appState.loadFromRustConfig(cfg)
        }
    }

    /// Inline SecureField revealed under "Recap API key" when the user
    /// opts in via the Add/Replace button. Same shape as
    /// `apiKeyEntryRow` (STT) and `llmKeyEntryRow` (LLM) — uniform UI
    /// across the three key sections per the user's "stessi componenti"
    /// rule. The Save action calls the shared
    /// `dimmy_save_llm_provider_key("recap", vendor, key)` FFI and
    /// re-reads the config so the `has_<vendor>_recap_key` snapshot
    /// flips → green ✓ surfaces.
    private var recapKeyEntryRow: some View {
        MacRow("Paste key", showsDivider: false) {
            SecureField("sk-…", text: $recapKeyInput)
                .textFieldStyle(.roundedBorder)
                .frame(width: 240)
                .onSubmit { saveRecapKeyAndRefresh() }
            Button("Save") { saveRecapKeyAndRefresh() }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .disabled(recapKeyInput.isEmpty)
        }
    }

    /// Wrap `saveRecapProviderKey` with the same close-the-field +
    /// refresh-AppState dance that `saveLlmKey` / `saveApiKey` do, so
    /// the row collapses back to the "Saved" + Replace… state after a
    /// successful save (without this the SecureField stayed open with
    /// the cleared buffer and looked like a no-op to the user).
    private func saveRecapKeyAndRefresh() {
        guard !recapKeyInput.isEmpty else { return }
        saveRecapProviderKey()
        showRecapKeyField = false
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
                    // Symmetric counterpart of `normalizeLlmUrlForAuth`:
                    // when the user picks a non-Anthropic preset while
                    // the dictation LLM was on subscription auth, snap
                    // `llmAuthMethod` back to "api_key". The Authentication
                    // picker only renders for Anthropic, so a stale
                    // "subscription" string after switching to Gemini /
                    // OpenAI / etc. silently hid the LLM API key card
                    // with no way for the user to flip it back. User
                    // report: "se in llm metto antropic e subs e poi
                    // cambio e metto gemini, non riesco più a inserire
                    // la apikey per llm."
                    let newTag = ProviderTagging.providerTag(forUrl: preset.apiUrl)
                    if newTag != "anthropic" && appState.llmAuthMethod == "subscription" {
                        appState.llmAuthMethod = "api_key"
                    }
                    persistConfig()
                }
            }
        )
    }

    private func persistConfig() {
        // MacOutputPage owns LLM provider + Recap settings. ALL its
        // saves must include both flags, otherwise the Picker /
        // Toggle / TextField interactions in this page are silently
        // dropped. Found 2026-05-15 — same bug pattern as Notion.
        DimmyCore.shared.setConfig(
            appState.toRustConfig(includeLlm: true, includeRecap: true)
        )
    }
}

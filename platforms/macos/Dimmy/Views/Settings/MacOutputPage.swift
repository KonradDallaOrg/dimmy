import AppKit
import SwiftUI

// Output, LLM enhancement style + provider + paste behaviour. Style
// chips replace the old Picker, coloured swatches make the dozen
// options scannable. Sub-mode (cloud / on-device) and provider routing
// follow the design's two-tier layout.

struct MacOutputPage: View {
    @ObservedObject var appState: AppState

    @State private var llmKeyInput: String = ""
    @State private var showLlmKeyField: Bool = false

    // Recap-vendor key save, separate field from the dictation
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

    /// Recap export folder (Obsidian / Drive / Dropbox sync). UI-only Mac
    /// preference; key must match
    /// MeetingPostProcessService.recapExportFolderKey ("recapExportFolder").
    /// Empty = disabled.
    @AppStorage("recapExportFolder") private var recapExportFolder: String = ""

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

    /// Anthropic dual-auth path: API key (api.anthropic.com) or
    /// Claude Code subscription (claude-code://default).
    private var isAnthropicLlm: Bool {
        llmProviderTag == "anthropic"
    }

    /// OpenAI dual-auth path: API key (api.openai.com) or ChatGPT
    /// (Codex) subscription (codex://default). Mirror of the Win
    /// `LlmUseSubscriptionCard` which is shown for both Anthropic +
    /// Claude CLI and OpenAI + Codex when the matching CLI is signed
    /// in. Until 2026-06-20 Mac only handled this via the
    /// `MacCodexCard` toggle on the Integrations page; the LLM
    /// settings page now offers the same choice inline.
    private var isOpenaiLlm: Bool {
        llmProviderTag == "openai"
    }

    /// Human-readable label for the STT provider, used in the
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
            ? "Claude Pro / Team / Max, routed through the local `claude` CLI. No API key needed."
            : "Direct Anthropic API key, pay-as-you-go billing."
    }

    /// Apply an Authentication-picker choice (`api_key` / `subscription`)
    /// for the current LLM provider. Just flips `llm_auth_method` —
    /// the URL and model selected by the user are PRESERVED. The Rust
    /// dispatcher already routes by `(vendor_from_url, auth_method)`:
    /// `auth_method=subscription` with an Anthropic URL → `claude` CLI,
    /// with an OpenAI URL → `codex` CLI (see `llm.rs::process_text`
    /// lines 566-573 + 608-615). Changing the URL or model here would
    /// reset the user's choice in the LLM dropdown — the exact bug
    /// reported 2026-06-20 ("perché cambi il menù del modello?!").
    /// Win parity: `LlmUseSubscriptionCard` on Win also only flips the
    /// flag, never touches the URL.
    private func applyLlmAuthChoice(_ method: String) {
        appState.llmAuthMethod = method
        persistConfig()
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            rewriteStyleGroup
            llmModeGroup
            recapModelGroup

            // Per settings-map.md: CLIPBOARD, MEETINGS folder and
            // ADVANCED LLM all live behind Advanced. Pasteboard is a
            // single toggle ("Keep in clipboard history") that 95%
            // of users never change. Meetings folder used to be on
            // the Debug page on Mac, the checklist puts it here.
            if appState.showAdvanced {
                pasteboardGroup
                meetingStorageGroup
                recapExportGroup
                advancedLlmGroup
            }
        }
    }

    // MARK: - Meeting storage directory
    //
    // Moved here from MacAdvancedPage so the Output page hosts every
    // setting that shapes meeting recaps and dictation routing, per
    // docs/dev/settings-redesign-checklist.md "Meetings folder = A
    // on Output". The Rust core still owns the directory, this row
    // just round-trips `meeting_storage_path` through `setConfig`.

    @State private var meetingStorageError: String? = nil

    private var meetingStorageGroup: some View {
        Group {
            MacGroupLabel(text: "Meetings storage")
            MacTile {
                MacRow(
                    "Storage folder",
                    description: meetingStorageDescription,
                    hint: "Where meeting recordings, transcripts and recaps are saved. Switching folders affects new meetings only.",
                    hintURL: URL(string: "https://dimmy.app/help/settings-overview"),
                    showsDivider: meetingStorageError != nil
                ) {
                    HStack(spacing: 8) {
                        Button("Browse...") { pickMeetingFolder() }
                            .controlSize(.small)
                            .buttonStyle(.borderedProminent)
                        Button("Reset") { persistMeetingStoragePath("") }
                            .controlSize(.small)
                            .disabled(appState.meetingStoragePath.isEmpty)
                            .help("Reset to the default location")
                    }
                }
                if let err = meetingStorageError {
                    MacRow("", description: err, showsDivider: false) { EmptyView() }
                }
            }
        }
    }

    // MARK: - Recap export folder (Obsidian / Drive / Dropbox sync)
    //
    // UI-only Mac preference (UserDefaults "recapExportFolder") — NOT a Rust
    // config field. After each recap save, MeetingPostProcessService copies
    // recap.md here as `<title> (<meeting-id>).md`. Empty = off. Win parity:
    // UiPreferences.RecapExportFolder + the Settings card in SettingsWindow.

    private var recapExportGroup: some View {
        Group {
            MacGroupLabel(text: "Export recaps")
            MacTile {
                MacRow(
                    "Export folder",
                    description: recapExportFolder.isEmpty
                        ? "Off, recaps stay in the app only."
                        : recapExportFolder,
                    hint: "Save a copy of each recap as a markdown file in a folder you choose. Point it at an Obsidian vault or a Google Drive, Dropbox or OneDrive folder to sync your recaps for free.",
                    showsDivider: false
                ) {
                    HStack(spacing: 8) {
                        Button("Browse...") { pickRecapExportFolder() }
                            .controlSize(.small)
                            .buttonStyle(.borderedProminent)
                        Button("Turn off") { recapExportFolder = "" }
                            .controlSize(.small)
                            .disabled(recapExportFolder.isEmpty)
                            .help("Stop exporting recaps to a folder")
                    }
                }
            }
        }
    }

    private func pickRecapExportFolder() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.canCreateDirectories = true
        panel.prompt = "Choose"
        panel.message = "Choose a folder to export recaps into"
        if !recapExportFolder.isEmpty {
            panel.directoryURL = URL(fileURLWithPath: recapExportFolder)
        }
        guard panel.runModal() == .OK, let url = panel.url else { return }
        guard isDirectoryWritable(url.path) else { return }
        recapExportFolder = url.path
    }

    private var meetingStorageDescription: String {
        let effective = DimmyCore.shared.meetingsDirURL?.path ?? "-"
        return appState.meetingStoragePath.isEmpty
            ? "\(effective), default"
            : effective
    }

    private func pickMeetingFolder() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.canCreateDirectories = true
        panel.prompt = "Choose"
        panel.message = "Choose where to store meeting recordings"
        if let current = DimmyCore.shared.meetingsDirURL { panel.directoryURL = current }
        guard panel.runModal() == .OK, let url = panel.url else { return }
        if !isDirectoryWritable(url.path) {
            meetingStorageError = "Can't write to '\(url.lastPathComponent)'. Pick a folder you have write access to."
            return
        }
        meetingStorageError = nil
        persistMeetingStoragePath(url.path)
    }

    private func persistMeetingStoragePath(_ path: String) {
        meetingStorageError = nil
        if DimmyCore.shared.setConfig(["meeting_storage_path": path]) {
            appState.meetingStoragePath = path
        }
    }

    private func isDirectoryWritable(_ dir: String) -> Bool {
        let fm = FileManager.default
        do {
            try fm.createDirectory(atPath: dir, withIntermediateDirectories: true)
            let probe = (dir as NSString).appendingPathComponent(".dimmy_write_probe")
            try "".write(toFile: probe, atomically: true, encoding: .utf8)
            try? fm.removeItem(atPath: probe)
            return true
        } catch {
            return false
        }
    }

    /// Recap effective provider tag, derived purely from the chosen
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

    /// Should we offer the subscription Authentication picker for
    /// recap? Only when the recap model's vendor has its CLI actually
    /// connected: Anthropic model → Claude Code signed in, OpenAI
    /// model → Codex signed in (Win parity: `canShowRecapSub`).
    /// Otherwise the picker is either irrelevant (other vendor) or
    /// non-actionable (no subscription connection yet).
    private var recapSubscriptionAvailable: Bool {
        (recapProviderTag == "anthropic" && appState.claudeCodeReady)
            || (recapProviderTag == "openai" && appState.codexReady)
    }

    /// True when a Recap-scope key for the chosen vendor is already
    /// saved in the keystore. Drives the green ✓ next to the recap
    /// key field. Mirrors Win `HasRecapKey` (SettingsWindow.xaml.cs:3771)
    /// which reads `has_<vendor>_recap_key`, the dedicated Recap
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
    /// Recap key UI is entirely hidden, the CLI handles its own auth.
    /// Win parity: SettingsWindow.xaml.cs:3686.
    /// Recap auth is INDEPENDENT of dictation since 2026-05-16, the
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

    /// Show the Recap API key entry row only for vendors NOT served by
    /// the Providers & keys page (Custom / unknown). For catalog vendors
    /// the Providers page is the single source of truth — it writes the
    /// key into BOTH `Llm(vendor)` and `Recap(vendor)` keystore scopes
    /// at once, so there's nothing for a per-recap-vendor key field to
    /// add. The legacy "Use same key as <vendor>" toggle was removed
    /// because it switched between two scopes that always hold the same
    /// value post-PR-99 (vestigial / confusing). Anthropic still gets
    /// its own subscription toggle above — that one swaps the auth
    /// METHOD (api_key vs Claude CLI subscription), a different choice.
    private var recapKeyFieldShouldShow: Bool {
        guard recapVendorKnown, !recapSubscriptionActive else { return false }
        // Catalog vendors → Providers page handles the key, no inline
        // entry needed here.
        let catalog: Set<String> = [
            "anthropic", "openai", "gemini",
            "groq", "fireworks", "together", "openrouter",
        ]
        return !catalog.contains(recapProviderTag)
    }

    /// Persist the recap-vendor's key via the dedicated FFI. Vendor
    /// is DERIVED from the chosen recap model (no separate provider
    /// control). Routes to `KeyringScope::Recap(<vendor>)` without
    /// touching the dictation llm_api_url. Empty key clears the entry.
    ///
    /// We deliberately DON'T guard on `vendor != llmProviderTag` , 
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
            // No explicit refresh needed, `recapKeyAlreadySaved`
            // reads `dimmy_get_config_json` on demand each render.
        } else {
            NSLog("[Auth] dimmy_save_llm_provider_key rc=\(rc) provider=\(vendor)")
        }
    }

    /// Resolve what "Auto" actually means right now, the dictation
    /// LLM's model id. Lets the picker show the inherited target
    /// instead of the opaque "match LLM provider" label.
    private var autoResolutionLabel: String {
        let m = appState.llmApiModel.trimmingCharacters(in: .whitespaces)
        return m.isEmpty ? "your dictation LLM (not yet configured)" : m
    }

    /// Description text shown under the Recap model row. Mirrors the
    /// CURRENT selection — not always the Auto behaviour. Burned
    /// 2026-06-20: a user picked gpt-5 and the row kept reading "Auto
    /// inherits claude-haiku" (the dictation LLM), which looked like
    /// the picker was overriding their choice.
    private var recapPickerDescription: String {
        if appState.recapPickerNeedsSelectPrompt {
            return "Connect a provider on the Providers and keys page, or pick a model below."
        }
        let override = appState.recapModelOverride.trimmingCharacters(in: .whitespaces)
        if override.isEmpty {
            return "Auto inherits \(autoResolutionLabel)."
        }
        if override.hasPrefix("local:") {
            return "Runs offline with the bundled model."
        }
        return "Recap runs with \(override)."
    }

    /// Annotate the "Auto" row in the dropdown with the actual model id
    /// it will dispatch to right now, so the user sees what's going to
    /// run without having to read the description below.
    private func displayLabel(for opt: RecapModelOption) -> String {
        opt.id.isEmpty ? "Auto, \(autoResolutionLabel)" : opt.label
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
                    description: recapPickerDescription,
                    hint: "Picks the model that summarises long dictations and meetings. Auto follows your dictation LLM.",
                    hintURL: URL(string: "https://dimmy.app/help/use-meeting-recaps"),
                    showsDivider: false
                ) {
                    Picker("", selection: Binding<String>(
                        get: { appState.recapModelOverride },
                        set: { newValue in
                            appState.recapModelOverride = newValue
                            persistConfig()
                        }
                    )) {
                        // Honest placeholder when no LLM provider is
                        // connected. Auto would silently fail in that
                        // state, so the picker prompts the user to pick
                        // something concrete (Local Gemma, or a Custom id
                        // they want to try). Tag matches the current
                        // value of `recap_model_override` (empty string)
                        // so SwiftUI's selection-binding contract holds:
                        // the picker renders the placeholder as the
                        // current row until the user makes a choice.
                        if appState.recapPickerNeedsSelectPrompt {
                            Label("Select a model...", systemImage: "questionmark.circle")
                                .tag("")
                        }
                        ForEach(appState.availableRecapModels()) { opt in
                            Label {
                                Text(displayLabel(for: opt))
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
                            Label("Custom, \(appState.recapModelOverride)",
                                  systemImage: "wrench.adjustable")
                                .tag(appState.recapModelOverride)
                        }
                    }
                    .labelsHidden()
                    .pickerStyle(.menu)
                    .frame(minWidth: 280)
                }

                // Subscription toggle for the recap call site —
                // only appears when the recap model's vendor has its
                // CLI connected (Anthropic → claude, OpenAI → codex).
                // For any other recap provider the toggle is hidden
                // entirely (user's rule: "se uso modello diverso da
                // antrop. non devo poter selezionare usa subs
                // atropic sotto"). Writes `recap_auth_method` so the
                // recap call routes through the matching local CLI
                // even when dictation stays on API key.
                if recapSubscriptionAvailable {
                    let recapSubIsOpenAI = recapProviderTag == "openai"
                    // Uniformed with the LLM "Authentication" picker in
                    // the LLM section below — same two-way segmented
                    // choice, same labels, same width. The previous
                    // Toggle framed it as opt-in ("Use Anthropic
                    // subscription for recap") which read inconsistently
                    // next to the LLM picker. Recap auth is independent
                    // of dictation since 2026-05-16; GET reads the
                    // literal `recap_auth_method` (empty == api_key,
                    // the default), SET writes an explicit value so
                    // config.json stays grep-able in logs.
                    MacRow(
                        "Authentication",
                        description: appState.recapAuthMethod == "subscription"
                            ? (recapSubIsOpenAI
                                ? "ChatGPT Plus / Pro / Team."
                                : "Claude Pro / Team / Max.")
                            : "Direct API key, pay-as-you-go.",
                        hint: recapSubIsOpenAI
                            ? "Sends the recap through your local Codex CLI instead of an API key. Dictation rewrite keeps its own auth method."
                            : "Sends the recap through your local Claude CLI instead of an API key. Dictation rewrite keeps its own auth method.",
                        hintURL: URL(string: "https://dimmy.app/help/integrations-claude-cli"),
                        showsDivider: recapKeyFieldShouldShow
                    ) {
                        Picker("", selection: Binding(
                            get: {
                                appState.recapAuthMethod == "subscription" ? "subscription" : "api_key"
                            },
                            set: { newValue in
                                appState.recapAuthMethod = newValue
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

                // Dedicated Recap-scope API key for vendors NOT in the
                // Providers & keys catalog (Custom / unknown). Catalog
                // vendors are handled via `recapKeyFieldShouldShow` which
                // returns false for them — the user pastes the key once
                // in Providers and it's written into BOTH `Llm(vendor)`
                // and `Recap(vendor)` scopes, so there's nothing to add
                // here. The "Use same key as <vendor>" toggle that used
                // to live between these blocks was removed post-PR-99
                // because the two scopes always hold the same value,
                // making the toggle a no-op that confused users.
                if recapKeyFieldShouldShow {
                    MacRow(
                        "Recap API key",
                        description: "Encrypted locally.",
                        hint: "Stored encrypted on this device with AES-256. Used only for the recap call.",
                        hintURL: URL(string: "https://dimmy.app/help/api-keys"),
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
                               ? (showRecapKeyField ? "Cancel" : "Replace...")
                               : (showRecapKeyField ? "Cancel" : "Add key...")) {
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
            MacGroupFooter(text: "Cloud entries reuse your LLM key. Local entries need a Gemma `.gguf` downloaded under Voice → Local model.")
        }
    }

    // MARK: Rewrite style (chip picker)

    private var rewriteStyleGroup: some View {
        Group {
            MacGroupLabel(text: "Rewrite style")
            MacTile {
                VStack(alignment: .leading, spacing: 12) {
                    HStack(spacing: 4) {
                        Text("Style").font(.system(size: 13))
                        MacInfoButton(
                            text: "An LLM cleans up or reshapes your transcript. Off leaves it exactly as spoken.",
                            url: URL(string: "https://dimmy.app/help/styles")
                        )
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
                MacRow(
                    "Mode",
                    hint: "Local runs the LLM on your device and stays private. Cloud sends your transcript to the provider you pick.",
                    hintURL: URL(string: "https://dimmy.app/help/local-llm")
                ) {
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
                    let availableLlmPresets = appState.availableLlmPresets()
                    // "Only Custom + Claude Code subscription" = the user
                    // has connected no cloud API keys at all. Surface a
                    // jump-to-Providers row so they don't get stuck.
                    let hasOnlyAlwaysAvailable = availableLlmPresets.allSatisfy {
                        $0.id == "custom"
                    }
                    MacRow(
                        "Provider",
                        hint: "Only providers you've connected on Providers and keys are shown. Claude Code subscription and Custom are always available.",
                        hintURL: URL(string: "https://dimmy.app/help/cloud-providers")
                    ) {
                        Picker("", selection: llmPresetBinding) {
                            ForEach(availableLlmPresets) { preset in
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

                    if hasOnlyAlwaysAvailable {
                        MacRow(
                            "",
                            description: "No cloud LLM providers connected yet.",
                            showsDivider: true
                        ) {
                            Button("Open Providers and keys") {
                                NotificationCenter.default.post(
                                    name: .dimmyNavigateSettingsTab,
                                    object: nil,
                                    userInfo: ["tab": "providers"]
                                )
                            }
                            .controlSize(.small)
                        }
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
                    if isAnthropicLlm || isOpenaiLlm {
                        // Anthropic (Claude Code subscription) and OpenAI
                        // (Codex / ChatGPT subscription) both have a CLI
                        // path that bypasses API-key billing. Same UI
                        // shape for both — only the labels + the URL
                        // swap target differ. Mirror of the Win
                        // `LlmUseSubscriptionCard` which already covers
                        // both providers (xaml.cs:1821-1826).
                        let isSubscription = appState.llmAuthMethod == "subscription"
                        let canSwitchToSubscription = isAnthropicLlm
                            ? (appState.claudeCodeReady || isSubscription)
                            : (appState.codexReady || isSubscription)
                        let vendorName = isAnthropicLlm ? "Claude" : "ChatGPT"
                        let subDescription = isAnthropicLlm
                            ? "Claude Pro / Team / Max."
                            : "ChatGPT Plus / Pro / Team."
                        let cliName = isAnthropicLlm ? "claude" : "codex"
                        let integrationHelp = isAnthropicLlm
                            ? "https://dimmy.app/help/integrations-claude-cli"
                            : "https://dimmy.app/help/integrations-codex"
                        if canSwitchToSubscription {
                            MacRow(
                                "Authentication",
                                description: isSubscription
                                    ? subDescription
                                    : "Direct API key, pay-as-you-go.",
                                hint: "Sends the rewrite through your local `\(cliName)` CLI instead of an API key. A few seconds slower per call, but no credit is used.",
                                hintURL: URL(string: integrationHelp),
                                showsDivider: isSubscription
                            ) {
                                Picker("", selection: Binding(
                                    get: { isSubscription ? "subscription" : "api_key" },
                                    set: { newValue in
                                        applyLlmAuthChoice(newValue)
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
                                description: "Connect \(vendorName) in Settings → Integrations.",
                                hint: "\(vendorName) subscription routes through the local `\(cliName)` CLI. Until that's signed in here, the LLM can only authenticate with a pay-as-you-go API key.",
                                showsDivider: true
                            ) {
                                EmptyView()
                            }
                        }
                    }

                    // Subscription branch: the actual sign-in /
                    // connection card lives in Settings → Integrations
                    // (Anthropic + Codex are connections like Notion,
                    // not pure provider configs). Here we just show a
                    // tiny "Manage in Integrations" pointer so users
                    // see the auth choice took effect.
                    let onSubscription = appState.llmAuthMethod == "subscription"
                        && (isAnthropicLlm || isOpenaiLlm)
                    if onSubscription {
                        let cliName = isAnthropicLlm ? "claude" : "codex"
                        let isReady = isAnthropicLlm ? appState.claudeCodeReady : appState.codexReady
                        let vendorLabel = isAnthropicLlm ? "Claude Code" : "Codex"
                        MacRow(
                            "Subscription connection",
                            description: isReady
                                ? "✓ Connected via Settings → Integrations."
                                : "⚠ Not connected, calls will fail.",
                            hint: isReady
                                ? "Sign-in and disconnect live in Settings → Integrations. The token is read on every LLM call via the local `\(cliName)` CLI; Dimmy never stores or sees its contents."
                                : "Subscription is selected but \(vendorLabel) isn't signed in yet. Open Settings → Integrations to connect, otherwise every LLM call will return an error.",
                            showsDivider: false
                        ) {
                            EmptyView()
                        }
                    } else {
                        // API-key branch. The legacy "Use my saved API
                        // key for this provider" toggle was removed —
                        // it switched between two scopes (Llm vs Stt)
                        // that always hold the same value post-PR-99
                        // (the Providers card writes to ALL scopes for
                        // a vendor at once), so the toggle had no
                        // observable effect on the dispatch path. The
                        // Rust dispatcher already falls back from
                        // Llm-scope to Stt-scope for the same vendor
                        // (see `ffi.rs::dimmy_process_with_llm`), so a
                        // single saved key always wins. Same precedent
                        // as the Recap "Use same key" toggle removed
                        // earlier (see `recapKeyFieldShouldShow`).
                        // The meaningful auth choice — API key vs
                        // Claude Code subscription — is the
                        // Authentication picker above, scoped to
                        // Anthropic.
                        do {
                            let currentLlmIsCustom = appState.llmApiUrl.isEmpty
                            if currentLlmIsCustom {
                                MacRow(
                                    "LLM API key",
                                    description: "Encrypted locally.",
                                    hint: "Stored encrypted on this device with AES-256. The provider only ever receives your transcript and this key, nothing else.",
                                    hintURL: URL(string: "https://dimmy.app/help/api-keys"),
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
                                    Button(appState.hasLlmKey ? (showLlmKeyField ? "Cancel" : "Replace...")
                                                              : (showLlmKeyField ? "Cancel" : "Add key...")) {
                                        showLlmKeyField.toggle()
                                        if !showLlmKeyField { llmKeyInput = "" }
                                    }
                                    .controlSize(.small)
                                }
                                if showLlmKeyField {
                                    llmKeyEntryRow
                                }
                            } else {
                                MacRow(
                                    "LLM API key",
                                    description: appState.hasLlmKey
                                        ? "Saved. Managed in Providers and keys."
                                        : "Not connected. Set it in Providers and keys.",
                                    hint: "Keys live in one place: Providers and keys. Connect once there, every page picks it up.",
                                    hintURL: URL(string: "https://dimmy.app/help/api-keys"),
                                    showsDivider: false
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
                                    Button("Manage on Providers and keys") {
                                        NotificationCenter.default.post(
                                            name: .dimmyNavigateSettingsTab,
                                            object: nil,
                                            userInfo: ["tab": "providers"]
                                        )
                                    }
                                    .controlSize(.small)
                                }
                            }
                        }
                    }
                } else {
                    MacRow(
                        "Local model",
                        hint: "Runs fully offline. Larger models read better but need more memory and time.",
                        hintURL: URL(string: "https://dimmy.app/help/local-llm"),
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
                            // Single source of truth: the Rust core's
                            // AVAILABLE_LLM_MODELS via dimmy_list_llm_models,
                            // loaded through LocalLlmCatalog — so adding a
                            // local model touches only core/src/local_llm.rs
                            // (matches Windows PopulateLocalLlmModels).
                            //
                            // Green ✓ next to entries already on disk
                            // mirrors the Voice → Local model picker
                            // (whisper/Parakeet). Presence query via
                            // `dimmy_llm_model_exists`; cheap, runs only
                            // when the picker re-renders.
                            ForEach(LocalLlmCatalog.models) { model in
                                if DimmyCore.shared.llmModelExists(model.filename) {
                                    Label(model.label, systemImage: "checkmark.circle.fill")
                                        .foregroundStyle(.green)
                                        .tag(model.filename)
                                } else {
                                    Text(model.label).tag(model.filename)
                                }
                            }
                        }
                        .labelsHidden()
                        .frame(width: 280)
                    }

                    if llmDownloadInFlight {
                        MacRow("Downloading \(appState.localLlmModel)...", showsDivider: false) {
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
            appState.refreshCodexStatus()
        }
    }

    private func refreshLocalLlmStatus() {
        guard DimmyCore.shared.isInitialized else {
            localLlmExists = false
            return
        }
        // Check via the listLLMModels payload, each entry has `downloaded`.
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
            SecureField("sk-...", text: $llmKeyInput)
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
        // includeLlm:true, user is wiring up the LLM key and the
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
    /// `apiKeyEntryRow` (STT) and `llmKeyEntryRow` (LLM), uniform UI
    /// across the three key sections per the user's "stessi componenti"
    /// rule. The Save action calls the shared
    /// `dimmy_save_llm_provider_key("recap", vendor, key)` FFI and
    /// re-reads the config so the `has_<vendor>_recap_key` snapshot
    /// flips → green ✓ surfaces.
    private var recapKeyEntryRow: some View {
        MacRow("Paste key", showsDivider: false) {
            SecureField("sk-...", text: $recapKeyInput)
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
    /// the row collapses back to the "Saved" + Replace... state after a
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
                    hint: "Leaves the transcription on the system clipboard after auto-paste so you can paste it again later.",
                    hintURL: URL(string: "https://dimmy.app/help/settings-overview"),
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

    // MARK: Advanced, tone, translate, custom prompt

    private var advancedLlmGroup: some View {
        Group {
            MacGroupLabel(text: "Advanced LLM")
            MacTile {
                MacRow(
                    "Tone",
                    hint: "Adjusts how formal or casual the LLM rewrite sounds. Applied on top of the active style.",
                    hintURL: URL(string: "https://dimmy.app/help/tones")
                ) {
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
                    hint: "Translates the LLM rewrite to this language. The pill's scroll wheel changes the same setting.",
                    hintURL: URL(string: "https://dimmy.app/help/use-multilingual")
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
                    HStack(spacing: 4) {
                        Text("Custom prompt")
                            .font(.system(size: 13))
                        MacInfoButton(
                            text: "Free-form instruction prepended to the LLM prompt when the active style is Custom. Use it to set persona, audience, or strict formatting rules.",
                            url: URL(string: "https://dimmy.app/help/custom-prompts")
                        )
                    }
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
                            .fill(Color.primary.opacity(0.04))
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
                    ?? "custom"
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
        // dropped. Found 2026-05-15, same bug pattern as Notion.
        DimmyCore.shared.setConfig(
            appState.toRustConfig(includeLlm: true, includeRecap: true)
        )
    }
}

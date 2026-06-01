# Mac handover — port the Windows settings redesign to macOS

Date: 2026-06-01. Branch with the Windows work: `feat/settings-redesign` (local only, not pushed). This doc is for porting that redesign to macOS (`platforms/macos`). The shared Rust core change is already done and committed.

## What shipped on Windows (the thing to mirror)
A full Settings UX pass. Read these for the full picture:
- `docs/dev/settings-map.md` — the compiled Simple/Advanced visibility map (the USER filled the "Vuoi" column; it is the source of truth for what goes where on Mac too).
- `docs/dev/settings-redesign-checklist.md` — nothing-lost mapping + as-shipped notes.

Headline changes:
1. **New "Providers & keys" page** — one card per provider (real brand logo, status pill, one key field, "Get key →" deep-link, model list with STT/LLM/Recap badges). One key per provider saved to all capable scopes. **This page does not exist on Mac yet — it's the biggest new build.**
2. **Info-ⓘ progressive disclosure** — each setting row shows a short one-line description inline; an ⓘ icon gives the longer text on hover (tooltip) and on click opens an in-app card with an "Open full guide →" link to `https://dimmy.app/help/<slug>` (41 real help pages exist).
3. **Simple/Advanced** — most settings moved behind an "Advanced mode" toggle; Simple shows only the essentials. Exact per-item placement is in `settings-map.md`.
4. **Copy** — every description rewritten short, plain, and **dash-free** (no em-dash —, en-dash –, or tilde ~; the user is adamant about this).
5. **Misc** — Home: stats above meeting; Meeting + Transcribe cards side-by-side; "WAV" → "audio file". "Advanced" page renamed to "Debug". "Made with Claude" footer removed.
6. **Alignment** — card glyph + title + ⓘ + control all vertically centered; info icon must not use the default control min-size (it floats the icon).

## Shared Rust core — ALREADY DONE (committed)
`dimmy_save_llm_provider_key(scope, provider, key)` now accepts scope **"stt"** for speech vendors (incl. Deepgram), plus "llm"/"recap" for completions vendors (`Provider::supports_stt()` in `core/src/provider.rs`, FFI in `core/src/ffi.rs`). So the Mac Providers page can save ONE key per provider across all scopes via this single FFI — no further core work needed. Deepgram is now keyable; only Custom (needs a base URL) stays inline on Voice/Output.

## Mac architecture (where things live)
- **`MacAtoms.swift`** → `MacRow` is the Mac `SettingCard` equivalent: `MacRow(label, description, icon, iconBackground, showsDivider, trailing)`. **No InfoTip/HelpUrl yet — extend it.** Other atoms: `MacTile`, `MacSquircleIcon`, `MacGroupLabel`, `MacGroupFooter`, `MacHero`, `MacChip`, `MacStatusPill`, `MacNote`.
- **`MacSettingsContainerView.swift`** → sidebar nav + main pane + **Advanced toggle already exists** (`appState.showAdvanced` gates the `.advanced` tab via `filteredTabs`; auto-navigates Home when toggled off on Advanced). Pages dispatched by `MacSettingsTab` enum switch. Per-page advanced gating already used (MacVoicePage, MacOutputPage).
- **Pages**: MacHomePage, MacVoicePage, MacOutputPage, MacPillPage, MacRulesPage, MacShortcutPage, MacIntegrationsPage, MacPrivacyPage, MacLicensePage, MacAboutPage, MacAdvancedPage. **No MacProvidersPage.**
- **Provider metadata**: only `Utilities/ProviderTagging.swift` (URL→vendor, recap-model→vendor). No `ProviderCatalog` equivalent. The curated lists are `SttPreset.presets`, `LlmPreset.presets`, `RecapModelOption.curated`.
- **Per-vendor key flags ALREADY on Mac**: `appState.hasGroqKey/...` (STT scope), `appState.llmKeyByVendor` (`has_<v>_llm_key`), `appState.recapKeyByVendor` (`has_<v>_recap_key`), loaded from `dimmy_get_config_json`. → connected-state is straightforward.
- **Keys today**: STT key → `config["api_key"]` via `DimmyCore.setConfig`; LLM key → `config["llm_api_key"]`; Recap → `dimmy_save_llm_provider_key("recap", vendor, key)`.
- **Theme**: `NSApp.appearance` (auto/light/dark) + `Color.primary.opacity(...)` tokens, auto light/dark. No custom palette needed.

## Port plan (suggested order)
1. **Extend `MacRow`** with `var infoTip: String? = nil` and `var helpUrl: String? = nil`. When set, render a trailing/inline `ⓘ` (SF Symbol `info.circle`) with `.help(infoTip)` (hover tooltip) and a tap that opens a small popover showing `infoTip` + a `Link("Open full guide →", destination: helpUrl)`. Keep it visually centered with the label (avoid default button min-sizes).
2. **Create `ProviderCatalog.swift`** (Utilities) mirroring `platforms/windows/Dimmy.Windows/Services/ProviderCatalog.cs`: id, name, accent, consoleUrl, stt/llm flags, getKeyHint, and the **exact same model lists** (groq/openai/anthropic/gemini/deepgram/openrouter/fireworks/together/local/custom). Add `supportsStt`/`keySaveScopes` helpers mirroring the C# logic (stt for speech vendors, llm+recap for completions vendors; Deepgram = stt only; custom/local = none).
3. **Create `MacProvidersPage.swift`** + add `.providers` to `MacSettingsTab` (Simple-visible, right after Output). Cards per provider: logo (reuse `Assets/Providers/*.svg` — copy into the Mac bundle, or use a white tile + Image), status pill driven by the real per-vendor flags (`hasXxxKey || llmKeyByVendor[v]`), one SecureField + Connect, "Get key →" link, model list with STT/LLM/Recap badges. On Connect: call `dimmy_save_llm_provider_key` for each scope in `keySaveScopes(provider)`. Deepgram now works; Custom shows a "set on Voice/Output" note.
4. **Apply the Simple/Advanced map** from `settings-map.md` to the Mac pages/tabs: gate the same nav tabs + in-page sections behind `appState.showAdvanced`. (Privacy + About are Simple; Pill/Rules/Recordings/Integrations/Debug are Advanced; within-page gating per the table.)
5. **Copy sweep** — shorten every `MacRow` description to one dash-free line; move the detail into `infoTip`; set `helpUrl` to the matching `https://dimmy.app/help/<slug>` (slug list in the checklist/Win XAML). NO em-dashes/tildes anywhere.
6. **Misc parity** — Home: stats above the meeting/transcribe cards; "WAV" → "audio file"; rename the Advanced tab/title to "Debug"; remove any "Made with Claude" credit on MacAboutPage.
7. **Optional follow-up** (same as Win): consider removing the now-redundant inline STT/LLM key fields on Voice/Output once Providers covers them — but keep the provider/model **routing** pickers (they set `api_url`/`llm_api_model`/`recap_model_override`). The Providers page does keys, not routing.

## Build / verify (MANDATORY, per CLAUDE.md)
- Run `scripts/dev/preflight-mac.sh` — it rebuilds the Rust static lib (Mac frozen features), runs `xcodebuild`, AND launches the app 5 s so `SelfTests.runAtLaunch` fires.
- If you add the Providers tab or change onboarding step counts / presets, **update `SelfTests` in the same commit** (it pins `LlmPreset`, `SttPreset`, onboarding step counts, etc.) or the DMG crashes on first launch.
- Keep copy dash-free; mirror the Win voice from the showcase (`MacVoicePage` first, get the user's OK, then sweep).

## Assets
Provider logos live in `platforms/windows/Dimmy.Windows/Assets/Providers/*.svg` (groq, openai, anthropic, gemini, deepgram, fireworks, openrouter, together, local, custom — local/custom are new monochrome marks). Copy/convert into the Mac asset catalog, or render via `Image`/SF Symbols. Rendered on a white rounded tile so monochrome marks stay visible in both themes.

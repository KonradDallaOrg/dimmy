# Mac parity handover — command mode + settings keys/filtering

Date: 2026-06-02. Win work lives on branch `feat/command-mode-hotkey` (merged into `staging` for the `v0.6.53-staging.14` cut). This doc maps every Win change to the Mac side so you can align `platforms/macos`. The shared Rust core changes are already done + committed — Mac just calls them.

Read first: `docs/dev/2026-06-01-mac-settings-redesign-handover.md` (the earlier settings-redesign port) and `docs/ARCHITECTURE.md`.

## 1. Command mode: generate-or-transform (Rust DONE, Mac UI to verify)

**Shared core (already shipped):** `dimmy_command_transform(selection, spoken, …)` now takes an **OPTIONAL** selection.
- With a selection → `llm::build_command_transform_prompt` (transform/replace), as before.
- With NO selection (empty/whitespace) → new `llm::build_command_generate_prompt` (generate text to INSERT at the cursor).
- Return-code contract unchanged except: `-1` now only for empty *spoken* (empty selection is allowed).

**Mac to do:** find the Swift command-mode stop handler (mirror of Win `TranscriptionService.StopAndCommandAsync` + `App.StopAndCommandTransform`). Today Mac likely short-circuits to "paste raw spoken text" or requires a selection when nothing is selected. Change it to **always call `dimmy_command_transform(selection ?? "", spoken)`** and paste the result — with a selection the paste replaces it, with none it inserts at the caret. Remove any no-selection early-return. (Win ref commits: `4d9b355`, core `llm.rs`/`ffi.rs`.)

## 2. Command mode pill colour persistence (Mac UI)

Win bug: the pill's style dot reverted to the LLM colour after one command even though command mode was still on. Fix = a single dot-colour setter that respects command mode, used everywhere.

**Mac to do:** wherever the Mac pill paints its style indicator, make it amber while command mode is active (the sticky menu toggle OR a transient one-shot — see §3), and make sure returning to idle doesn't overwrite it. (Win ref: `PillWindow.RefreshStyleDot`, commit `0cd25cc`.)

## 3. Dedicated command-mode hotkey — ONE-SHOT vs sticky (Mac UI + AppState)

Design (agreed with user): the **pill/menu toggle = persistent** command mode (stays on until toggled off); a **dedicated hotkey = one-shot** (runs one command, then reverts to normal output). Opt-in, empty by default.

**Win mechanism:** a 2nd global hotkey via Win32 `RegisterHotKey` (key-down only → toggle: press start, press/Stop stop), independent of the main hotkey's low-level hook. A transient `CommandOneShot` flag routes the stop to the command path + paints the pill amber, and self-clears after the command (the sticky toggle is untouched). Config in `UiPreferences.CommandHotkey` (ui_prefs, not config.json). Recorder requires modifier+key; toast on RegisterHotKey conflict.

**Mac mechanism is DIFFERENT:** Mac already runs a `CGEventTap` for the main hotkey + the dictionary hotkey. Add the command hotkey the same Mac way you added the dictionary hotkey (NSEvent global monitor / CGEventTap), NOT RegisterHotKey. Mac can support modifier-only combos on the tap if you want (Win can't — that's a RegisterHotKey limitation, not a product decision). Store the combo in the Mac UI-prefs equivalent. Add an `appState.commandOneShot` (mirror of `CommandOneShot`) that the pill + the stop-routing read; clear it after the command. (Win ref: `CommandHotkeyService.cs`, `App.OnCommandHotkeyTriggered`, commit `4d9b355` + hotkey-validation `9574398`.)

## 4. Settings: keys ONLY in Providers + filtered model pickers (Mac — biggest item)

Win change: the STT / LLM / Recap sections no longer take an API key. Keys live only in the Providers & keys page (one key per provider, saved to every capable scope). The STT/LLM/Recap sections are now **model pickers filtered to the providers you have a key for** (+ local + custom always; Anthropic stays under subscription). Custom endpoint keeps an inline URL+key. The "use my saved key" inheritance toggles are gone; the subscription toggles stay.

**Mac to do:**
- Drop the inline API-key fields from MacVoicePage / MacOutputPage (keep them only for the Custom endpoint).
- Filter the provider/model pickers to connected providers (`has_<vendor>_*key`). Recap tags are model ids → derive the vendor (claude→anthropic, gpt/o3→openai, gemini→gemini); keep Auto/Custom/local always.
- Keep the subscription toggles.

### ⚠️ CRITICAL PITFALL — read this, Mac very likely has the SAME bug

On Win the provider "Connected" state and the picker filter must read the **LIVE keystore from the FFI** (`dimmy_get_config_json` → `has_<vendor>_key` / `has_<vendor>_llm_key`), NOT from the saved `config.json`. **`config.json` does NOT contain the `has_*_key` flags — the Rust core computes them live and strips them on save.** On Win, one code path read them from the config.json file → all-false → the Providers page silently fell back to a UI mirror and disagreed with the dropdowns (a provider with a real key showed in the pickers but said "Add key").

Check `platforms/macos/.../AppState.swift`: if `appState.hasGroqKey` / `llmKeyByVendor` / `recapKeyByVendor` are populated by decoding the **config.json file**, they're all-false and wrong. They MUST come from the **FFI snapshot** (`dimmy_get_config_json`). Use that single FFI source for BOTH the Providers page Connected pills AND the picker filters so they can never disagree. Re-read the FFI flags after a Connect/Remove so the UI updates immediately. (Win ref commit: `c1c3c24`.)

"Connected" = a key in ANY scope (the same provider key works for STT, LLM, recap), so e.g. a Groq speech key also unlocks Groq's rewrite/recap models.

Note: a leftover key in `keys.enc` legitimately shows its provider as Connected — that's correct (the FFI is the truth). The user removes it via Providers → Remove.

## 5. Hotkey recorder validation (Mac — minor)

Win: the shared ShortcutRecorder gained a `RequireKey` mode for the RegisterHotKey-based hotkeys (dict + command) — rejects modifier-only / unmappable combos with a hint. On Mac the equivalent recorder may be more permissive; ensure the command/dict hotkey recorders only accept combos the Mac global-hotkey mechanism can actually bind.

## Build / verify (MANDATORY)
`scripts/dev/preflight-mac.sh` — rebuilds the Rust static lib (Mac frozen features), `xcodebuild`, AND launches the app 5 s so `SelfTests.runAtLaunch` fires. If you add a Providers/command-hotkey surface or change presets, update `SelfTests` in the same commit or the DMG crashes on first launch. Keep copy dash-free (the user is adamant — see the no-em-dash rule).

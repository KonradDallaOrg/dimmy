# Settings redesign — nothing-lost checklist

> **Status, checked against the code on 2026-11-11:** the redesign shipped on
> Windows and is no longer a branch. Exists today: the Providers page
> (`ProviderCatalog.cs`, `SettingsWindow.Providers.cs`), the sidebar "Advanced
> mode" toggle (`IsAdvanced`), the mic device in Simple, no live-preview widget
> on Windows, the four auth toggles hidden (cards forced to Collapsed in
> `SettingsWindow.xaml.cs`). Differs from the tables below: STT/LLM mode, local
> model, cloud provider and Custom endpoint controls were NOT folded into
> Providers (they stay on Voice input / Output); only API keys moved (Custom
> keeps its key field on Voice input / Output). Pill overlay, App rules,
> Recordings and Debug are Advanced-gated nav pages; Integrations and Privacy
> are NOT gated any more. The per-task routing dropdowns were never built.
> `UiPreferences.ConnectedProviders` does not exist (connected state is read
> from the keystore). Mac has the same pages (`MacProvidersPage.swift`). The
> "New home" column is the original plan; rows with "(today: ...)" show where
> the code differs. Current page map: `docs/dev/settings-map.md`.

Source of truth for the Simple/Advanced redesign on `feat/settings-redesign`.
Every pre-existing setting must end up in **Simple**, **Advanced**, the new
**Providers** page, or be **Removed (with reason)**. The verification agent
checks this list against the rebuilt UI.

Decisions (locked):
- Visual = native WinUI Fluent, theme-aware (light+dark). NOT a bespoke dark theme.
- Backend = unchanged Rust core. New UI maps onto existing config fields + keystore.
- Win only this branch; Mac parity is a follow-up.

Legend: **S**=Simple · **A**=Advanced · **P**=Providers page · **R**=Removed

## Voice input page (was)
| Setting | New home | Note |
|---|---|---|
| Native language | S | essential |
| STT Mode (Local/Cloud) | P | folds into Providers (per-task STT pick) (today: stayed on Voice input, S) |
| Local STT model + Download | P | model pick under On-device in Providers (today: Voice input, S, local mode; the Providers On-device card also lists models) |
| Remove filler words | A | default on (today: Voice input, S, local mode only) |
| Cloud STT provider | P | becomes a provider card (today: Voice input combo, S, cloud mode) |
| Cloud STT API key | P | per-provider key |
| Custom endpoint URL/model | P | Custom provider card (today: Voice input, only with Provider = Custom) |
| Recognition prompt | A | |
| **Input device (mic)** | **S** | **PROMOTED — was wrongly under Advanced** |
| Microphone volume | A | |
| Preprocessing | A | default on |
| Chunk streaming (Parakeet) | A | auto when Parakeet (today: "Accelerate transcription", A) |
| Live captions | A | |
| Custom dictionary (add/list) | A | |

## Shortcut page (was)
| Setting | New home | Note |
|---|---|---|
| Recording shortcut | S | the single most essential control |
| PTT/Toggle mode | S | |
| Add-to-dictionary shortcut | A | (today: S, Shortcut page) |

## Output page (was)
| Setting | New home | Note |
|---|---|---|
| LLM Style (incl. off) | S | the AI on/off gateway |
| LLM Mode (Local/Cloud) | P | folds into Providers (today: Output, S) |
| Local LLM model + Download | P | under On-device card (today: Output, S, local mode) |
| Cloud LLM provider + model | P | provider card + per-task LLM pick (today: Output provider combo, S, cloud mode) |
| Use Anthropic subscription (LLM) | A | kept (niche auth) (today: Output, shown conditionally, not Advanced-gated) |
| Custom LLM endpoint/model | P | Custom card (today: Output, only with Custom) |
| Use my saved API key (LLM) | R | auto — one key per provider |
| LLM API key | P | per-provider key (today: Providers; Output shows it only for Custom) |
| Detect calls + offer record | A | (was mis-placed on Output) |
| Excluded apps | A | |
| Meetings folder | A | |
| Meeting recap model | P | per-task Recap pick (today: Output, S, not Advanced-gated) |
| Use Anthropic subscription (recap) | A | kept but inherits by default (today: Output, conditional) |
| Custom recap model id | P | Custom-only (today: Output, shown when the recap model is Custom) |
| Use my saved API key (recap) | R | auto — recap reuses provider key |
| Recap API key | R | reuse provider key |
| Keep in clipboard | A | |
| Tone | A | already advanced |
| Translate output to | A | already advanced (pill is primary) |
| Custom prompt | A | |

## Pill overlay page (was)
| Setting | New home | Note |
|---|---|---|
| Show pill on start | A | |
| Show pill when recording | A | |
| Show Dimmy in taskbar | A | |
| Default position + Reset | A | |
| Border style | A | KEPT (user wants pill settings) |
| Waveform style | A | KEPT |
| **Live preview demo widget** | **R** | not a setting — demo only (user agreed) |

## App rules page (was)
| Setting | New home | Note |
|---|---|---|
| Add rule / Load defaults | A | |
| Rule rows (pattern/style/translate/enabled/reorder/delete) | A | |

## Recordings page (was)
| Setting | New home | Note |
|---|---|---|
| Search | A | |
| History list + detail (copy/delete/waveform/player) | A | content, kept |
| Save audio with history | A | |
| Audio retention days | A | nested under Save audio |
| Audio quota MB | A | nested under Save audio |

## Home page (was)
| Setting | New home | Note |
|---|---|---|
| Hero + time-saved stats | S | KEPT (user wants it) |
| Record meeting / Transcribe file | S | quick actions |
| Theme (Auto/Light/Dark) | A | |
| Launch at login | S | first-run decision (today: Home "System" group, Advanced-gated) |

## Integrations page (was)
| Setting | New home | Note |
|---|---|---|
| Notion connect/auto-send | A | (today: Integrations, always visible) |
| Anthropic subscription connect/test | A | (also surfaced in Providers) |
| MCP / Claude Desktop | A | (today: Integrations, always visible) |

## Privacy & data page (was)
| Setting | New home | Note |
|---|---|---|
| Privacy promise card | A | (today: shown in Simple; Privacy nav is not gated) |
| Send anonymous usage data | A | consent (default on, opt-out) |
| Send crash reports | A | consent |
| Anonymous ID + Reset | A | |
| Send feedback (kind/text/email) | A | (today: shown in Simple) |
| **Enable & send** | **REWORK** | becomes a modal "enable crash reports & send?" |
| Privacy policy / What we collect links | A | (today: shown in Simple) |

## License / About / Advanced (was)
| Setting | New home | Note |
|---|---|---|
| License hero/devices/buy/activate | License | meta page, kept as-is |
| Version / Check updates / release notes | About | meta page, kept |
| Update channel | About/A | (today: About, shown only with the auto_update license scope) |
| Diagnostics (llm log / audio debug / ggml) | A | |
| GPU status + Retry | A | |

## Removed (with reason)
- Live-preview pill demo widget — not a setting, visual demo only.
- 4 auth toggles (llm_use_same_key, recap_use_same_key, recap key, separate recap scope) — **UI removed; backend fields untouched, defaulted to auto** (one key per provider). No config field deleted → nothing lost on disk.
- "Enable & send" button → modal flow.

## Providers page — new (the centerpiece)
Provider cards (11 in `ProviderCatalog.All`): On-device, Groq, OpenAI, Anthropic, Gemini, Deepgram, OpenRouter, Fireworks, Together, Kimi Code, Custom.
Capabilities: Deepgram=STT only; Anthropic+OpenRouter+Kimi Code=LLM/Recap only; Groq, OpenAI, Gemini, Fireworks, Together=STT+LLM+Recap; On-device=all (no key); Custom=STT+LLM (key entered on Voice input / Output, not on its card).
Per provider: status pill · key field · Connect/Remove (no Verify) · "Get key →" deep-link · models w/ STT/LLM/Recap badges.
Advanced override: per-task STT/LLM/Recap dropdown (capable+connected models) + fallback. (NOT BUILT: no such dropdown in `SettingsWindow.Providers.cs`.)
Maps to existing config: STT→api_url/api_model, LLM→llm_api_url/llm_api_model, Recap→recap_model_override; keys→keystore FFI.

## Implementation notes (as shipped on `feat/settings-redesign`, 2026-06-01)

What actually landed (5 local commits, nothing pushed; build green; 279
C# tests pass incl. 19 new ProviderCatalog guards; app launches clean):

1. **Providers & keys page** — additive nav page driven by `ProviderCatalog.cs`
   + `SettingsWindow.Providers.cs` (cards built programmatically, no XAML
   DataTemplate). One key per provider → saved to all capable scopes
   (stt/llm/recap) via `dimmy_save_llm_provider_key`. Per-provider HTTPS
   "Get key →" deep-link. Connected state is read from the live keystore through FFI (`IsProviderConnected`); nothing is mirrored in `UiPreferences`.
2. **Mic device promoted** to Simple; gain/preprocessing/chunk/captions stay Advanced.
3. **Live-preview pill demo removed** (widget + ~180 lines render code-behind).
4. **Nav-level progressive disclosure** — Pill overlay / App rules / Recordings / Debug
   gated behind the always-visible "Advanced mode" footer toggle (`IsAdvanced`,
   default false; Integrations and Privacy were gated at first, are not any more).
   Daily-use pages stay in Simple.
5. **Enable & send** kept as the existing contextual inline button (appears
   only on rc -2) + tooltip; NOT converted to a modal (a modal interrupts
   more — anti-enshittification). 3 verbose descriptions trimmed.

**Verification agent verdict: PASS** — all 72 staging bindings still reachable,
no orphaned handlers, no dangling x:Name. Nothing lost.

**Deliberately deferred (NOT done — discuss before doing):**
- (Today: API keys for catalog providers are entered only on the Providers
  page; Voice/Output show a key field only for Custom. Provider and model
  selection still live on Voice/Output, and the routing dropdowns below were
  not built.) Original note: the redesign is **additive, not relocating**:
  STT/LLM/recap provider+key
  controls still lived in the Voice/Output panels AND on the Providers page,
  so a key could be entered in two places. This was the SAFE choice (guarantees
  "nothing lost / works at least as well as now"). De-duplicating requires
  first building the **per-task routing dropdowns** on the Providers page
  (the mockup's "Avanzate · override per capability" section) so Voice/Output
  can surrender provider *selection*, not just key entry. Until then, removing
  the in-panel controls would lose the ability to pick which provider/model
  serves each task.
- Per-provider key **validation** ("Verify" / inline ✓) — currently optimistic
  save. The deep-link wizard is "get a key", not "prove the key works".
- **Mac parity** — this branch was Windows-only. Done since: `MacProvidersPage.swift` and the same tab set in `MacSettingsContainerView.swift`.

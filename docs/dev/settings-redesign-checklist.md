# Settings redesign — nothing-lost checklist

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
| STT Mode (Local/Cloud) | P | folds into Providers (per-task STT pick) |
| Local STT model + Download | P | model pick under On-device in Providers |
| Remove filler words | A | default on |
| Cloud STT provider | P | becomes a provider card |
| Cloud STT API key | P | per-provider key |
| Custom endpoint URL/model | P | Custom provider card |
| Recognition prompt | A | |
| **Input device (mic)** | **S** | **PROMOTED — was wrongly under Advanced** |
| Microphone volume | A | |
| Preprocessing | A | default on |
| Chunk streaming (Parakeet) | A | auto when Parakeet |
| Live captions | A | |
| Custom dictionary (add/list) | A | |

## Shortcut page (was)
| Setting | New home | Note |
|---|---|---|
| Recording shortcut | S | the single most essential control |
| PTT/Toggle mode | S | |
| Add-to-dictionary shortcut | A | |

## Output page (was)
| Setting | New home | Note |
|---|---|---|
| LLM Style (incl. off) | S | the AI on/off gateway |
| LLM Mode (Local/Cloud) | P | folds into Providers |
| Local LLM model + Download | P | under On-device card |
| Cloud LLM provider + model | P | provider card + per-task LLM pick |
| Use Anthropic subscription (LLM) | A | kept (niche auth) |
| Custom LLM endpoint/model | P | Custom card |
| Use my saved API key (LLM) | R | auto — one key per provider |
| LLM API key | P | per-provider key |
| Detect calls + offer record | A | (was mis-placed on Output) |
| Excluded apps | A | |
| Meetings folder | A | |
| Meeting recap model | P | per-task Recap pick |
| Use Anthropic subscription (recap) | A | kept but inherits by default |
| Custom recap model id | P | Custom-only |
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
| Launch at login | S | first-run decision |

## Integrations page (was)
| Setting | New home | Note |
|---|---|---|
| Notion connect/auto-send | A | |
| Anthropic subscription connect/test | A | (also surfaced in Providers) |
| MCP / Claude Desktop | A | |

## Privacy & data page (was)
| Setting | New home | Note |
|---|---|---|
| Privacy promise card | A | |
| Send anonymous usage data | A | consent (default on, opt-out) |
| Send crash reports | A | consent |
| Anonymous ID + Reset | A | |
| Send feedback (kind/text/email) | A | |
| **Enable & send** | **REWORK** | becomes a modal "enable crash reports & send?" |
| Privacy policy / What we collect links | A | |

## License / About / Advanced (was)
| Setting | New home | Note |
|---|---|---|
| License hero/devices/buy/activate | License | meta page, kept as-is |
| Version / Check updates / release notes | About | meta page, kept |
| Update channel | About/A | |
| Diagnostics (llm log / audio debug / ggml) | A | |
| GPU status + Retry | A | |

## Removed (with reason)
- Live-preview pill demo widget — not a setting, visual demo only.
- 4 auth toggles (llm_use_same_key, recap_use_same_key, recap key, separate recap scope) — **UI removed; backend fields untouched, defaulted to auto** (one key per provider). No config field deleted → nothing lost on disk.
- "Enable & send" button → modal flow.

## Providers page — new (the centerpiece)
Provider cards: Groq, OpenAI, OpenRouter, Gemini, Deepgram, Anthropic, Fireworks, Together, On-device, Custom.
Capabilities: Deepgram=STT only; Anthropic+OpenRouter=LLM/Recap only; rest=STT+LLM+Recap; On-device=all (no key).
Per provider: status pill · key field · Connect/Verify/Remove · "Get key →" deep-link · models w/ STT/LLM/Recap badges.
Advanced override: per-task STT/LLM/Recap dropdown (capable+connected models) + fallback.
Maps to existing config: STT→api_url/api_model, LLM→llm_api_url/llm_api_model, Recap→recap_model_override; keys→keystore FFI.

## Implementation notes (as shipped on `feat/settings-redesign`, 2026-06-01)

What actually landed (5 local commits, nothing pushed; build green; 279
C# tests pass incl. 19 new ProviderCatalog guards; app launches clean):

1. **Providers & keys page** — additive nav page driven by `ProviderCatalog.cs`
   + `SettingsWindow.Providers.cs` (cards built programmatically, no XAML
   DataTemplate). One key per provider → saved to all capable scopes
   (stt/llm/recap) via `dimmy_save_llm_provider_key`. Per-provider HTTPS
   "Get key →" deep-link. Connected-state mirrored in `UiPreferences.ConnectedProviders`.
2. **Mic device promoted** to Simple; gain/preprocessing/chunk/captions stay Advanced.
3. **Live-preview pill demo removed** (widget + ~180 lines render code-behind).
4. **Nav-level progressive disclosure** — App rules / Integrations / Privacy
   gated behind the always-visible "Advanced mode" footer toggle (`IsAdvanced`,
   default false). Daily-use pages stay in Simple.
5. **Enable & send** kept as the existing contextual inline button (appears
   only on rc -2) + tooltip; NOT converted to a modal (a modal interrupts
   more — anti-enshittification). 3 verbose descriptions trimmed.

**Verification agent verdict: PASS** — all 72 staging bindings still reachable,
no orphaned handlers, no dangling x:Name. Nothing lost.

**Deliberately deferred (NOT done — discuss before doing):**
- The redesign is **additive, not relocating**: STT/LLM/recap provider+key
  controls still live in the Voice/Output panels AND on the Providers page,
  so a key can be entered in two places. This was the SAFE choice (guarantees
  "nothing lost / works at least as well as now"). De-duplicating requires
  first building the **per-task routing dropdowns** on the Providers page
  (the mockup's "Avanzate · override per capability" section) so Voice/Output
  can surrender provider *selection*, not just key entry. Until then, removing
  the in-panel controls would lose the ability to pick which provider/model
  serves each task.
- Per-provider key **validation** ("Verify" / inline ✓) — currently optimistic
  save. The deep-link wizard is "get a key", not "prove the key works".
- **Mac parity** — this branch is Windows-only.

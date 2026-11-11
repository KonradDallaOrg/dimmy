# Settings — mappa completa (per compilare la visibilità Simple/Advanced)

> Mappa di OGNI menù e OGNI voce interna di `SettingsWindow.xaml`, in ordine di
> pagina. Compila tu la colonna **Vuoi** (S = Simple / A = Advanced) e correggi
> nome/testo come preferisci. Nessuna voce persa.
>
> Verificata contro `SettingsWindow.xaml` e `SettingsWindow.xaml.cs` il 2026-11-11:
> la colonna **Ora** riflette il codice di oggi (le scelte della colonna **Vuoi**
> sono in gran parte già applicate). La sidebar ha anche una casella
> "Search settings" (`NavSearchBox`) e, nel footer, il toggle "Advanced mode"
> (`IsAdvanced` in `SettingsViewModel.cs`, non persistito: parte sempre a Off).
>
> Legenda colonna **Ora** (stato attuale nel codice):
> - `S` = visibile in Simple (sempre)
> - `A-nav` = pagina intera nascosta dietro il toggle "Advanced mode"
> - `A-sez` = sezione nascosta dietro Advanced dentro una pagina Simple
> - `cond` = visibile solo in certe condizioni (es. modalità Cloud, provider Custom)
> - `(display)` = non è un setting, è testo/stato/risultato (incluso per completezza)
>
> Colonna **Semplif.**: Sì = il testo descrittivo è lungo/tecnico e lo accorcerei.

## Menù (nav) — riepilogo
Ordine reale della sidebar. La voce "Debug" ha `Tag="advanced"` nel codice.

| Menù | Ora | Vuoi | Nome alternativo proposto |
|---|---|---|---|
| Home | S | | — |
| Voice input | S | | "Voce" / "Dettatura" |
| Output | S | | "Riscrittura" / "Testo finale" |
| Providers & keys | S | | "Provider e chiavi" |
| Pill overlay | A-nav | | "Pillola" / "Overlay" |
| App rules | A-nav | | "Regole per app" |
| Shortcut | S | | "Scorciatoia" |
| Recordings | A-nav | | "Registrazioni" / "Cronologia" |
| Integrations | S | | "Integrazioni" |
| Privacy & data | S | S | "Privacy" |
| License | S | | "Licenza" |
| About | S | S | "Info" |
| Debug | A-nav | | "Debug" |

---

## Home  (nav: S)
| Voce | Tipo | Ora | Vuoi | Nome alternativo | Semplif. |
|---|---|---|---|---|---|
| Hero (titolo + riquadri Shortcut / Transcription / Microphone attuali) | display | S | | — | No |
| YOUR DICTATION — stats (parole / tempo / risparmio) | display | S | | "Le tue statistiche" | No |
| RECENT — ultime trascrizioni + "See all" (apre Recordings) | display | S | | "Recenti" | No |
| SET UP MORE — card "Meetings" e "Command mode" (wizard; la sezione si nasconde se le card non servono) | azione | cond | | — | No |
| MEETING MODE — "Record a meeting" + Open meeting window | azione | S | | "Riunioni" / "Registra una riunione" | Sì (testo card lungo) |
| TRANSCRIBE A FILE — drop + Pick file | azione | S | | "Trascrivi un file" | No |
| TRANSCRIBE A FILE — Run recap as meeting | azione | S | | "Genera recap dal file" | No |
| SYSTEM — Theme (Auto/Light/Dark) | radio | A-sez | A-sez | "Tema" | No |
| SYSTEM — Launch at login | toggle | A-sez | A-sez | "Avvia all'accesso" | No |

## Providers & keys  (nav: S)
| Voce | Tipo | Ora | Vuoi | Nome alternativo | Semplif. |
|---|---|---|---|---|---|
| Intro pagina | display | S | | — | No |
| Card per provider (logo, stato, key, get-key, modelli): On-device, Groq, OpenAI, Anthropic, Google Gemini, Deepgram, OpenRouter, Fireworks, Together AI, Kimi Code, Custom (elenco in `ProviderCatalog.cs`; le card sono costruite in `SettingsWindow.Providers.cs`) | cards | S | | — | No |
| Footnote "chiavi cifrate sul dispositivo, legate a questo computer e account" | display | S | | — | No |

## Voice input  (nav: S)
| Voce | Tipo | Ora | Vuoi | Nome alternativo | Semplif. |
|---|---|---|---|---|---|
| LANGUAGE — Native language | combo | S | | "Lingua parlata" | No |
| SPEECH-TO-TEXT — Mode (Local/Cloud) | radio | S | | "Motore: locale o cloud" | No |
| SPEECH-TO-TEXT — Local model | combo | S (cond local) | | "Modello locale" | Sì |
| SPEECH-TO-TEXT — Model status + Download | azione | S (cond local) | | "Scarica modello" | No |
| SPEECH-TO-TEXT — Remove filler words | toggle | S (cond local) | | "Togli intercalari" | No |
| SPEECH-TO-TEXT — Provider (cloud) | combo | S (cond cloud) | | "Provider STT" | No |
| SPEECH-TO-TEXT — API key | password | cond custom (le chiavi dei provider del catalogo si inseriscono in Providers & keys) | | "Chiave STT" | No |
| SPEECH-TO-TEXT — Custom endpoint URL | text | cond custom | | — | No |
| SPEECH-TO-TEXT — Custom model | text | cond custom | | — | No |
| VOCABULARY — Recognition prompt | text | A-sez + cond cloud | A-sez | "Contesto/vocabolario" | No |
| SPEAKERS — Identify speakers in meetings | toggle | S | | "Riconosci chi parla" | No |
| SPEAKERS — Speaker model + Download (104 MB) | azione | cond (toggle acceso) | | — | No |
| MICROPHONE — Input device | combo | S | | "Microfono" | No |
| MICROPHONE — Microphone volume | slider | **A-sez** | | "Volume mic" | No |
| MICROPHONE — Preprocessing | toggle | **A-sez** | | "Pre-elaborazione audio" | No |
| MICROPHONE — Accelerate transcription (`ChunkStreamingEnabled`; Parakeet in tempo reale, whisper richiede GPU) | toggle | **A-sez** | | "Streaming a blocchi" | Sì |
| MICROPHONE — Live captions (on screen) | toggle | **A-sez** | | "Sottotitoli live" | No |
| MICROPHONE — Realtime typing (`StreamingDictation`) | toggle | **A-sez** | | "Scrittura in tempo reale" | Sì |
| CUSTOM DICTIONARY — add/list/remove | lista | A-sez | A-sez | "Dizionario personale" | No |

## Output  (nav: S)
| Voce | Tipo | Ora | Vuoi | Nome alternativo | Semplif. |
|---|---|---|---|---|---|
| LLM ENHANCEMENT — Style (incl. Off) | combo | S | | "Stile di riscrittura" | No |
| LLM MODE — Mode (Local/Cloud) | radio | S | | "Motore LLM: locale/cloud" | No |
| LLM MODE — Local LLM model | combo | S (cond local) | | "Modello LLM locale" | No |
| LLM MODE — Local LLM status + Download | azione | S (cond local) | | — | No |
| LLM MODE — Cloud LLM provider | combo | S (cond cloud) | | "Provider LLM" | No |
| LLM MODE — Use Anthropic subscription | toggle | cond | | "Usa abbonamento Claude" | Sì |
| LLM MODE — Custom LLM endpoint | text | cond custom | | — | No |
| LLM MODE — Custom LLM model | text | cond custom | | — | No |
| LLM MODE — Use my saved API key | toggle | nascosto sempre (`LlmUseSameKeyCard` è forzata a Collapsed in `SettingsWindow.xaml.cs`; il core legge comunque la chiave `llm` poi `stt` del vendor) | | — | — |
| LLM MODE — LLM API key | password | cond custom (le chiavi dei provider del catalogo stanno in Providers & keys) | | "Chiave LLM" | No |
| AUTO-DETECT MEETINGS — Detect calls + offer | toggle | A-sez | A-sez | "Rileva chiamate" | No |
| AUTO-DETECT MEETINGS — Start recording automatically | toggle | A-sez (attivo solo con Detect calls acceso; la prima attivazione chiede conferma) | A-sez | "Registra in automatico" | No |
| AUTO-DETECT MEETINGS — Excluded apps | lista | A-sez | A-sez | "App escluse" | No |
| MEETINGS — Meetings folder + Reset/Browse | azione | A-sez | A-sez | "Cartella riunioni" | No |
| MEETINGS — Export recaps to a folder + Browse/Turn off | azione | A-sez | A-sez | "Esporta recap in una cartella" | Sì |
| MEETING RECAP — Recap every meeting | toggle | S | | "Recap di ogni riunione" | No |
| MEETING RECAP — Meeting recap model | combo | S | | "Modello del recap" | No |
| MEETING RECAP — Use Anthropic subscription for recap | toggle | cond | | "Recap con abbonamento Claude" | Sì |
| MEETING RECAP — Custom recap model id | text | cond | | — | No |
| MEETING RECAP — Use my saved API key (recap) | toggle | nascosto sempre (`RecapUseSameKeyCard`) | | — | — |
| MEETING RECAP — Recap API key | password | nascosto sempre (`RecapKeyCard`: la chiave `recap` si salva in Providers & keys) | | — | — |
| CLIPBOARD — Keep in clipboard history | toggle | A-sez | A-sez | "Tieni negli appunti" | No |
| ADVANCED LLM — Tone | combo | **A-sez** | | "Tono" | No |
| ADVANCED LLM — Translate output to | combo | **A-sez** | | "Traduci l'output" | No |
| ADVANCED LLM — Custom prompt | text | **A-sez** | | "Prompt personalizzato" | No |

> Nota: AUTO-DETECT MEETINGS, MEETINGS (folder + export) e CLIPBOARD sono già
> dietro Advanced. In Simple restano **Style**, **Mode**, provider/modello e tutta
> la sezione MEETING RECAP (il pannello `RecapPanel` non è dietro Advanced).

## Shortcut  (nav: S)
| Voce | Tipo | Ora | Vuoi | Nome alternativo | Semplif. |
|---|---|---|---|---|---|
| HOTKEY — Recording shortcut | recorder | S | | "Scorciatoia di registrazione" | No |
| HOTKEY — Mode (PTT/Toggle) | radio | S | | "Modalità tasto" | No |
| HOTKEY — Command mode shortcut (opzionale, si può svuotare) | recorder | S | | "Scorciatoia comando" | Sì |
| HOTKEY — Meeting recording shortcut (opzionale, si può svuotare) | recorder | S | | "Scorciatoia riunione" | Sì |
| DICTIONARY — Add-to-dictionary shortcut | recorder | S | | "Scorciatoia dizionario" | Sì |

## License  (nav: S)
| Voce | Tipo | Ora | Vuoi | Nome alternativo | Semplif. |
|---|---|---|---|---|---|
| Stato licenza (hero) | display | S | | — | No |
| Refresh now / Sign out / Manage subscription | azione | S | | — | No |
| Devices (lista + reload + sign-out per device) | lista | S | | "Dispositivi" | No |
| Buy: Monthly / Annual / Supporter (nel codice il bottone è `LicenseBuyLifetimeButton`, etichetta "Supporter", "once · 3 years") | azione | S (cond) | | "Acquista" | No |
| Capabilities ("Included with your license") | display | S | | — | No |
| Activate (email + magic link) | azione | S | | "Attiva" | No |
| Paste activation code (Expander) | azione | S | | "Incolla codice" | No |

## Pill overlay  (nav: **A-nav**)
| Voce | Tipo | Ora | Vuoi | Nome alternativo | Semplif. |
|---|---|---|---|---|---|
| VISIBILITY — Show pill on app start | toggle | A-nav | | "Mostra all'avvio" | Sì |
| VISIBILITY — Show pill when recording | toggle | A-nav | | "Mostra in registrazione" | Sì |
| VISIBILITY — Show Dimmy in Windows taskbar | toggle | A-nav | | "Mostra nella taskbar" | No |
| VISIBILITY — Pin icon to the system tray | toggle | A-nav | | "Icona sempre visibile nella tray" | No |
| POSITION — Default position (griglia) | radio | A-nav | | "Posizione" | No |
| POSITION — Reset position | azione | A-nav | | — | No |
| APPEARANCE — Border style | combo | A-nav | | "Bordo" | No |
| APPEARANCE — Waveform style | combo | A-nav | | "Onda" | No |

## App rules  (nav: **A-nav**)
| Voce | Tipo | Ora | Vuoi | Nome alternativo | Semplif. |
|---|---|---|---|---|---|
| Add rule / Load defaults | azione | A-nav | | — | No |
| Lista regole (pattern, stile, traduzione, on/off, ordina, elimina) | lista | A-nav | | — | No |

## Recordings  (nav: **A-nav**)
| Voce | Tipo | Ora | Vuoi | Nome alternativo | Semplif. |
|---|---|---|---|---|---|
| Search + Refresh | azione | A-nav | | "Cerca" | No |
| Save audio with history | toggle | A-nav | | "Salva l'audio" | Sì |
| Audio retention (days) | number | A-nav | | "Giorni di conservazione" | No |
| Audio quota (MB) | number | A-nav | | "Limite spazio (MB)" | No |
| Lista + dettaglio (raw/enhanced, waveform, player, copy, delete) | lista | A-nav | | — | No |

## Integrations  (nav: **S**, sempre visibile)
| Voce | Tipo | Ora | Vuoi | Nome alternativo | Semplif. |
|---|---|---|---|---|---|
| CALENDAR — Outlook / Google Calendar: toggle + Connect the calendar / Check again | azione | S | | "Calendario" | No |
| NOTION — connect/change destination/disconnect + status | azione | S | | — | No |
| NOTION — Auto-send each meeting | toggle | S | | "Invia ogni riunione" | Sì |
| CONFLUENCE — set up wizard / test / change space / re-run setup / disconnect | azione | S | | — | No |
| CONFLUENCE — Auto-send each meeting | toggle | S | | "Invia ogni riunione" | Sì |
| ANTHROPIC SUBSCRIPTION — sign in/test/refresh/wizard | azione | S | | "Abbonamento Claude" | No |
| OPENAI SUBSCRIPTION (ChatGPT / Codex) — sign in/test/refresh/wizard | azione | S | | "Abbonamento ChatGPT" | No |
| KIMI (Kimi Code) — sign in/test/refresh/wizard; Kimi Work — disconnect/refresh | azione | S | | — | No |
| GOOGLE (Gemini CLI) — sign in/test/refresh/wizard | azione | S | | — | No |
| CLAUDE DESKTOP (MCP) — connect/disconnect/refresh | azione | S | | "Claude Desktop (MCP)" | No |
| TELEGRAM — login (QR code o numero + codice, password 2FA) / log out / "Transcribe and recap automatically" | azione | S | | — | No |

## Privacy & data  (nav: **S**) 
| Voce | Tipo | Ora | Vuoi | Nome alternativo | Semplif. |
|---|---|---|---|---|---|
| Privacy promise (card) | display | S | (display) | — | No |
| TELEMETRY — Send anonymous usage data | toggle | A-sez | A-sez | "Dati d'uso anonimi" | No |
| TELEMETRY — Send crash reports | toggle | A-sez | A-sez | "Segnalazioni crash" | No |
| ANONYMOUS IDENTIFIER — ID + Reset | azione | A-sez | A-sez | "ID anonimo" | No |
| SEND FEEDBACK — Kind / message / email / Send | azione | S | S | "Invia feedback" | No |
| SEND FEEDBACK — Enable & send (cond: appare solo se l'invio torna rc -2) | azione | cond | S | — | No |
| RESOURCES — Privacy policy / What we collect | link | S | S | — | No |
| AI TRANSPARENCY — How Dimmy uses AI | link | S | | — | No |

## About  (nav: **S**)
| Voce | Tipo | Ora | Vuoi | Nome alternativo | Semplif. |
|---|---|---|---|---|---|
| Hero (versione, logo) | display | S | (display) | — | No |
| Check for updates / View release notes | azione | S | S | "Controlla aggiornamenti" | No |
| Update ready (Install & restart / Release notes) | azione | cond | | — | No |
| Update channel (Stable only / Stable + Pre-release) | combo | cond (nascosto senza lo scope di licenza `auto_update`) | | "Canale aggiornamenti" | No |
| GUIDED SETUP — First run / Meetings / Command mode | azione | S | | — | No |
| RESOURCES — Source code (il link "Website" è stato tolto apposta: policy Microsoft Store 10.1.5) | link | S | S | — | No |
| LEGAL — Licence (AGPL-3.0) / Open source notices | azione | S | | — | No |

## Advanced (Debug)  (nav: **A-nav**, titolo pagina "Debug")
| Voce | Tipo | Ora | Vuoi | Nome alternativo | Semplif. |
|---|---|---|---|---|---|
| DIAGNOSTICS — LLM log enabled | toggle | A-nav | | — | No |
| DIAGNOSTICS — Audio debug | toggle | A-nav | | — | No |
| DIAGNOSTICS — ggml debug logging | toggle | A-nav | | — | No |
| GPU ACCELERATION — GPU status + Retry GPU | azione | A-nav | | "Accelerazione GPU" | No |

---

## Note di architettura (per decidere bene)
- **Chiavi API — una sola superficie per i provider del catalogo.** La pagina
  *Providers & keys* salva la chiave in `keys.enc` (cifrato) su **tutti** gli scope
  che il vendor supporta: `stt` per i vendor con endpoint vocale (groq, openai,
  gemini, deepgram, fireworks, together), `llm` + `recap` per quelli con endpoint
  di completions (`ProviderCatalog.KeySaveScopes`). L'FFI
  `dimmy_save_llm_provider_key` (`core/src/ffi.rs`) accetta lo scope `stt` per i
  vendor che supportano STT, quindi anche Deepgram si imposta in Providers.
  - Solo **Custom** (serve un URL base) tiene la chiave in *Voice input* (STT) e in
    *Output* (LLM), accanto al suo URL: `SttApiKeyCard` e `LlmApiKeyCard` compaiono
    solo con Custom.
  - Gli interruttori "Use my saved API key" e il campo "Recap API key" non si
    vedono più (card forzate a Collapsed); il core legge la chiave `llm` e poi
    quella `stt` dello stesso vendor.

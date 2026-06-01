# Settings — mappa completa (per compilare la visibilità Simple/Advanced)

> Mappa di OGNI menù e OGNI voce interna di `SettingsWindow.xaml`, in ordine di
> pagina. Compila tu la colonna **Vuoi** (S = Simple / A = Advanced) e correggi
> nome/testo come preferisci. Nessuna voce persa.
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
| Menù | Ora | Vuoi | Nome alternativo proposto |
|---|---|---|---|
| Home | S | | — |
| Voice input | S | | "Voce" / "Dettatura" |
| Output | S | | "Riscrittura" / "Testo finale" |
| Providers & keys | S | | "Provider e chiavi" |
| Shortcut | S | | "Scorciatoia" |
| License | S | | "Licenza" |
| Pill overlay | A-nav | | "Pillola" / "Overlay" |
| App rules | A-nav | | "Regole per app" |
| Recordings | A-nav | | "Registrazioni" / "Cronologia" |
| Integrations | A-nav | | "Integrazioni" |
| Privacy & data | A-nav | S | "Privacy" |
| About | A-nav | S | "Info" |
| Advanced | A-nav | | "Debug" |

---

## Home  (nav: S)
| Voce | Tipo | Ora | Vuoi | Nome alternativo | Semplif. |
|---|---|---|---|---|---|
| Hero "Welcome to Dimmy" | display | S | | — | No |
| YOUR DICTATION — stats (parole / tempo / risparmio) | display | S | | "Le tue statistiche" | No |
| MEETING MODE — "Record a meeting" + Open meeting window | azione | S | | "Riunioni" / "Registra una riunione" | Sì (testo card lungo) |
| TRANSCRIBE A FILE — drop + Pick file | azione | S | | "Trascrivi un file" | No |
| TRANSCRIBE A FILE — Run recap as meeting | azione | S | | "Genera recap dal file" | No |
| SYSTEM — Theme (Auto/Light/Dark) | radio | S | A-sez | "Tema" | No |
| SYSTEM — Launch at login | toggle | S | A-sez | "Avvia all'accesso" | No |

## Providers & keys  (nav: S)
| Voce | Tipo | Ora | Vuoi | Nome alternativo | Semplif. |
|---|---|---|---|---|---|
| Intro pagina | display | S | | — | No |
| Card per provider (logo, stato, key, get-key, modelli) | cards | S | | — | No |
| Footnote "chiavi cifrate AES-256" | display | S | | — | No |

## Voice input  (nav: S)
| Voce | Tipo | Ora | Vuoi | Nome alternativo | Semplif. |
|---|---|---|---|---|---|
| LANGUAGE — Native language | combo | S | | "Lingua parlata" | No |
| SPEECH-TO-TEXT — Mode (Local/Cloud) | radio | S | | "Motore: locale o cloud" | No |
| SPEECH-TO-TEXT — Local model | combo | S (cond local) | | "Modello locale" | Sì |
| SPEECH-TO-TEXT — Model status + Download | azione | S (cond local) | | "Scarica modello" | No |
| SPEECH-TO-TEXT — Remove filler words | toggle | S | | "Togli intercalari" | No |
| SPEECH-TO-TEXT — Provider (cloud) | combo | S (cond cloud) | | "Provider STT" | No |
| SPEECH-TO-TEXT — API key (cloud) | password | S (cond cloud) | | "Chiave STT" | No |
| SPEECH-TO-TEXT — Custom endpoint URL | text | cond custom | | — | No |
| SPEECH-TO-TEXT — Custom model | text | cond custom | | — | No |
| VOCABULARY — Recognition prompt | text | S (cond cloud) | A-sez | "Contesto/vocabolario" | No |
| MICROPHONE — Input device | combo | S | | "Microfono" | No |
| MICROPHONE — Microphone volume | slider | **A-sez** | | "Volume mic" | No |
| MICROPHONE — Preprocessing | toggle | **A-sez** | | "Pre-elaborazione audio" | No |
| MICROPHONE — Chunk streaming (Parakeet only) | toggle | **A-sez** | | "Streaming a blocchi" | Sì |
| MICROPHONE — Live captions | toggle | **A-sez** | | "Sottotitoli live" | No |
| CUSTOM DICTIONARY — add/list/remove | lista | S | A-sez | "Dizionario personale" | No |

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
| LLM MODE — Use my saved API key | toggle | cond | | "Riusa la chiave salvata" | Sì |
| LLM MODE — LLM API key | password | cond | | "Chiave LLM" | No |
| AUTO-DETECT MEETINGS — Detect calls + offer | toggle | S | A-sez | "Rileva chiamate" | No |
| AUTO-DETECT MEETINGS — Excluded apps | lista | S | A-sez | "App escluse" | No |
| MEETINGS — Meetings folder + Reset/Browse | azione | S | A-sez | "Cartella riunioni" | No |
| MEETING RECAP — Meeting recap model | combo | S | | "Modello del recap" | No |
| MEETING RECAP — Use Anthropic subscription for recap | toggle | cond | | "Recap con abbonamento Claude" | Sì |
| MEETING RECAP — Custom recap model id | text | cond | | — | No |
| MEETING RECAP — Use my saved API key (recap) | toggle | cond | | "Riusa la chiave salvata" | Sì |
| MEETING RECAP — Recap API key | password | cond | | "Chiave recap" | No |
| CLIPBOARD — Keep in clipboard history | toggle | S | A-sez | "Tieni negli appunti" | No |
| ADVANCED LLM — Tone | combo | **A-sez** | | "Tono" | No |
| ADVANCED LLM — Translate output to | combo | **A-sez** | | "Traduci l'output" | No |
| ADVANCED LLM — Custom prompt | text | **A-sez** | | "Prompt personalizzato" | No |

> Nota: la pagina Output ha MOLTE voci in Simple (call-detection, meetings folder,
> recap, ecc.). Candidati forti a passare in `A-sez`: AUTO-DETECT MEETINGS,
> MEETINGS folder, gran parte di MEETING RECAP. In Simple resterebbe lo stretto
> necessario: **Style**, **Mode**, provider/modello, **Keep in clipboard**.

## Shortcut  (nav: S)
| Voce | Tipo | Ora | Vuoi | Nome alternativo | Semplif. |
|---|---|---|---|---|---|
| HOTKEY — Recording shortcut | recorder | S | | "Scorciatoia di registrazione" | No |
| HOTKEY — Mode (PTT/Toggle) | radio | S | | "Modalità tasto" | No |
| DICTIONARY — Add-to-dictionary shortcut | recorder | S | | "Scorciatoia dizionario" | Sì |

## License  (nav: S)
| Voce | Tipo | Ora | Vuoi | Nome alternativo | Semplif. |
|---|---|---|---|---|---|
| Stato licenza (hero) | display | S | | — | No |
| Refresh now / Sign out / Manage subscription | azione | S | | — | No |
| Devices (lista + reload + sign-out per device) | lista | S | | "Dispositivi" | No |
| Buy: Monthly / Annual / Lifetime | azione | S (cond) | | "Acquista" | No |
| Capabilities ("Included with your license") | display | S | | — | No |
| Activate (email + magic link) | azione | S | | "Attiva" | No |
| Paste activation code (Expander) | azione | S | | "Incolla codice" | No |

## Pill overlay  (nav: **A-nav**)
| Voce | Tipo | Ora | Vuoi | Nome alternativo | Semplif. |
|---|---|---|---|---|---|
| VISIBILITY — Show pill on app start | toggle | A-nav | | "Mostra all'avvio" | Sì |
| VISIBILITY — Show pill when recording | toggle | A-nav | | "Mostra in registrazione" | Sì |
| VISIBILITY — Show Dimmy in Windows taskbar | toggle | A-nav | | "Mostra nella taskbar" | No |
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

## Integrations  (nav: **A-nav**)
| Voce | Tipo | Ora | Vuoi | Nome alternativo | Semplif. |
|---|---|---|---|---|---|
| NOTION — connect/change/disconnect + status | azione | A-nav | | — | No |
| NOTION — Auto-send each meeting | toggle | A-nav | | "Invia ogni riunione" | Sì |
| ANTHROPIC SUBSCRIPTION — sign in/test/refresh/wizard | azione | A-nav | | "Abbonamento Claude" | No |
| CLAUDE DESKTOP (MCP) — connect/disconnect/refresh | azione | A-nav | | "Claude Desktop (MCP)" | No |

## Privacy & data  (nav: **S**) 
| Voce | Tipo | Ora | Vuoi | Nome alternativo | Semplif. |
|---|---|---|---|---|---|
| Privacy promise (card) | display | A-nav | (display) | — | No |
| TELEMETRY — Send anonymous usage data | toggle | A-nav | A-sez | "Dati d'uso anonimi" | No |
| TELEMETRY — Send crash reports | toggle | A-nav | A-sez | "Segnalazioni crash" | No |
| ANONYMOUS IDENTIFIER — ID + Reset | azione | A-nav | A-sez | "ID anonimo" | No |
| SEND FEEDBACK — Kind / message / email / Send | azione | A-nav | S | "Invia feedback" | No |
| SEND FEEDBACK — Enable & send (cond) | azione | A-nav | S | — | No |
| RESOURCES — Privacy policy / What we collect | link | A-nav | S | — | No |

## About  (nav: **S**)
| Voce | Tipo | Ora | Vuoi | Nome alternativo | Semplif. |
|---|---|---|---|---|---|
| Hero (versione, logo) | display | A-nav | (display) | — | No |
| Check for updates / Release notes | azione | A-nav | S | "Controlla aggiornamenti" | No |
| Update channel (Stable / Pre-release) | combo | A-nav (cond) | | "Canale aggiornamenti" | No |
| RESOURCES — Website / Source code | link | A-nav | S | — | No |

## Advanced (Debug)  (nav: **A-nav**)
| Voce | Tipo | Ora | Vuoi | Nome alternativo | Semplif. |
|---|---|---|---|---|---|
| DIAGNOSTICS — LLM log enabled | toggle | A-nav | | — | No |
| DIAGNOSTICS — Audio debug | toggle | A-nav | | — | No |
| DIAGNOSTICS — ggml debug logging | toggle | A-nav | | — | No |
| GPU ACCELERATION — GPU status + Retry GPU | azione | A-nav | | "Accelerazione GPU" | No |

---

## Note di architettura (per decidere bene)
- **Chiavi API — due superfici, stesso keystore.** La pagina *Providers & keys*
  e i campi key in *Voice input / Output* scrivono nello stesso `keys.enc`
  cifrato, ma su **scope diversi**. La pagina Providers salva solo `llm`+`recap`
  (l'FFI rifiuta lo scope `stt` e i vendor deepgram/custom). Quindi:
  - la **chiave STT** di un provider (es. Groq usato per la voce) si imposta
    ancora in *Voice input*;
  - **Deepgram** (solo STT) e **Custom** si impostano in *Voice input / Output*.
- Questo è il motivo per cui i campi key in Voice/Output **non sono ridondanti**.
  Per renderli davvero ridondanti servirebbe estendere l'FFI Rust (accettare
  scope `stt` + deepgram/custom) — vedi domanda aperta.

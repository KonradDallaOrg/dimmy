# Wispr Flow vs Dimmy: tasti e onboarding (Windows)

Analisi del 2026-10-31 su Wispr Flow 1.6.1337. Nessuna modifica al codice. Le decisioni in fondo sono ancora aperte.

## Come è stata fatta

- Flow è Electron. Bundle letto da `%LOCALAPPDATA%\WisprFlow\app-1.6.1337\resources\app.asar` (`npx @electron/asar extract`), file `.webpack\main\index.js`.
- `resources\Release\Wispr Flow Helper.exe` è .NET: lette solo le stringhe, quindi i dati sull'helper sono meno sicuri.
- Screenshot: `Pictures\Screenshots`, `Screenshot 2026-10-31 *.png` (25 file, onboarding, Home, Impostazioni).
- Lato Dimmy: `core/src/hotkey.rs`, `Services/HotkeyService.cs`, `App.xaml.cs` (`OnHotkeyPressed`/`OnHotkeyReleased`), `Views/OnboardingWindow.xaml`.

## Topic 1: tasti e shortcut

In Flow l'utente non sceglie tra toggle e push-to-talk. Lo stesso tasto fa tutte e due le funzioni. La registrazione parte subito alla pressione, quindi il gesto non aggiunge ritardo.

| Gesto | Risultato in Flow |
|---|---|
| Tieni premuto e rilascia dopo almeno 500 ms | Ferma e incolla (`key_release`) |
| Tocco singolo breve | Attende 500 ms un secondo tocco, poi annulla (`DebouncePTT`) e scrive "Don't tap. Hold down {{shortcut}}." |
| Doppio tocco entro 500 ms | Blocca a mani libere (`isLocked`, nel codice "POPO") |
| Altro tocco a registrazione bloccata | Ferma e incolla (`re_press`) |
| Tre tocchi rapidi | Annulla la dettatura (`DebounceTriplePress`) e passa al Notetaker (`notetakerTripleTapShortcutEnabled`) |
| Esc | Annulla |

Costanti: `DICTATION_DEBOUNCE_DELAY = 500`, `SCRATCHPAD_PTT_DELAY_MS = 350`.

Default Windows di Flow: `Ctrl+Win` push-to-talk, `Ctrl+Win+Space` mani libere, `Ctrl+Win+Alt` Lens, `Shift+Alt+Z` incolla l'ultimo testo, `Shift+Alt+X` copia l'ultimo testo, `Esc` annulla. L'onboarding propone Caps Lock. Passare da `Ctrl+Win` a `Ctrl+Win+Space` a metà pressione non riavvia la dettatura ("Shortcut upgrade").

| Punto | Flow | Dimmy su Windows |
|---|---|---|
| Modalità | Gesti sullo stesso tasto | Impostazione globale `shortcut_mode`: `hold` o `toggle` |
| Combinazioni | Tasto singolo (Caps Lock), accordi, tasti del mouse | Almeno due tasti, con almeno un modificatore (`parse_combo`) |
| Scorciatoie per azione | Fino a 4 | 1 |
| Annullo con Esc | Sì | Non trovato |
| Suoni | `dictation-start`, `dictation-stop`, `paste`, `popo-lock` | Non trovati nel codice C# |
| Hook | Helper .NET, eventi `KeypressEvent` via IPC, logica in JS | Hook in Rust, thread C# che legge ogni 10 ms |
| Tasti bloccati | `CheckStaleKeys` sui tasti premuti da oltre 2,5 s | `forget_released_keys` |
| Menu Start e tasto Alt | Sequenza di mascheramento | Blocco dei tasti e key-up sintetici (`MODIFIER_SUPPRESS`) |

### Proposta

1. Doppio tocco: aggiungi il blocco alla modalità `hold` e lascia `toggle` invariato. Logica in una macchina a stati Rust testabile, valida per tutti i sistemi operativi.
2. Esc: annulla la registrazione, intercettando il tasto solo mentre Dimmy registra.
3. Tocco breve: messaggio che spiega come usare il tasto.
4. Tasto singolo (Caps Lock o F13): solo dopo i punti 1-3.
5. Da non fare: quattro scorciatoie per azione e tasti del mouse. Tre tocchi per il meeting: da decidere.

Rischio: l'evento dell'hook ha un solo slot (`Binding.event`). Un tocco più breve di 10 ms può perdere la pressione. Correggere prima del doppio tocco.

## Topic 2: onboarding e UI

Dimmy oggi ha cinque passi: benvenuto, locale o cloud, shortcut, prova, email.

Problemi, in ordine di importanza:

1. Scelta difficile troppo presto: il secondo passo chiede modello, chiave API e download prima di mostrare il valore del prodotto.
2. Nessun test del microfono.
3. Linguaggio tecnico: "Push-to-Talk / Toggle", "gsk_...".
4. Aspetto generico: il logo è un cerchio colorato (`Ellipse`), controlli standard centrati, cinque puntini.
5. Fine brusca: nessuna guida nella prima app reale.

Flusso di Flow visto negli screenshot: interessi (Dictation, Notetaker) → lavoro → permessi → test microfono → test shortcut → finta chat Slack → finto ChatGPT → "Nice job, 4x faster" → slider "32 hours a week" → trial Pro → referral → "Dictate 100 words" → consiglio sulla pill.

Da riprendere:

- Test con conferma: "Vedi le barre mentre parli?" con "Sì" e "Cambia microfono". Il tasto sullo schermo si illumina quando lo premi.
- Una decisione per schermata: domanda a sinistra, anteprima a destra.
- Prova in una scena realistica: finta chat al posto di un riquadro vuoto.
- Fasi con un nome: barra con Microfono, Shortcut, Prova, invece dei puntini.
- Consiglio sulla pill dopo la procedura: "Clicca in un campo di testo e tieni premuto…".
- Identità visiva: sfondo caldo, un solo colore di accento, un font con carattere per i titoli, illustrazioni nostre.
- Checklist nella Home per i passi opzionali (Meeting, Command mode), al posto delle schede nel wizard.
- Impostazioni raggruppate: Flow divide tra app e account, Dimmy ha 13 voci in un solo elenco.

Flusso proposto (sei passi):

1. Benvenuto con logo vero, frase breve e termini.
2. Test del microfono.
3. Test del tasto, con il gesto "tieni premuto".
4. Prova in una finta chat, con un motore già pronto.
5. Successo, e il doppio tocco se approvato.
6. Email facoltativa e fine.

Rischio: il passo 4 dipende dal motore. Un download locale obbligatorio prima della prova rovina il flusso. Provider e chiave API possono diventare una scelta successiva, ma serve un modello piccolo subito o un download in background.

Da non copiare: domanda "Che lavoro fai?", stima "32 ore a settimana", referral nell'onboarding, serie di giorni e classifiche, illustrazioni e testi di Flow (protetti da copyright).

## Fluent

Fluent è il design system di Microsoft. I controlli WinUI 3 lo usano di default, quindi è l'aspetto nativo di Windows 11. Flow è Electron e disegna tutto da solo: per questo sembra uguale ovunque e non sembra Windows.

Decisione consigliata: tieni WinUI 3 e Fluent, e su Mac SwiftUI. Il problema di oggi sono i default lasciati senza cura, non Fluent. Puoi aggiungere identità dentro WinUI con colori, font, illustrazioni e layout, solo in onboarding e Home. Le Impostazioni restano standard.

## Topic 3: note su meeting e dettature (richieste dal proprietario, 2026-10-31)

### Proseguire un meeting finito

- Flow: pulsante "Resume" sotto la trascrizione di un meeting finito (screenshot `011608`).
- Dimmy: `dimmy_meeting_resume` riprende solo da una pausa. Non ho trovato un modo di proseguire un meeting già fermato.
- Vincolo: tocca `meeting.rs`. Leggere prima `docs/dev/meeting-audio-durability.md` e THE AUDIO RULE.
- Da definire: come unire nuovo audio, tracce, trascrizione e recap al meeting esistente.

### Cronologia delle dettature a tabella

- Flow (screenshot `012141`): gruppi per giorno, ora in una colonna a sinistra, testo a destra, separatori orizzontali.
- Il proprietario la giudica molto meglio. Da rifare in Dimmy con Fluent, con più cura. Pagina: `HistoryPanel` in `SettingsWindow.xaml`.

### Trascrizione live del meeting formattata

- Richiesta: già durante la registrazione, timestamp colorato, speaker con colori e barrette, separatori orizzontali, come nella trascrizione finale.
- Dimmy oggi: il live è un testo semplice (`TranscriptText` + `TranscriptPartial`). `TranscriptRenderer.Render` viene chiamato per la vista finale.
- Punto di partenza: riusare `TranscriptRenderer` e `SpeakerColor`, per avere un solo aspetto.
- Vincolo: è lavoro solo di UI, niente nel loop di cattura. Verificare le prestazioni con meeting lunghi, senza rifare tutto il rendering a ogni riga.

### Play accanto a timestamp e speaker: non farlo

- Flow ha un piccolo play accanto allo speaker (screenshot `011608`).
- Dimmy porta già l'audio al punto giusto con la forma d'onda e con lo scorrimento della trascrizione (`SeekWaveformToPointer`, `SeekToScrolledTranscript`).
- Decisione del proprietario: probabilmente non serve. Non aggiungerlo.

### HIPAA

- HIPAA è la legge USA sui dati sanitari. Vale per chi gestisce dati di pazienti (ospedali, medici, assicurazioni) e per i loro fornitori che trattano quei dati. Il BAA è il contratto tra loro.
- Flow offre il BAA (screenshot `012103`). Il suo modello usa un cloud proprio per la dettatura.
- Dimmy: le trascrizioni restano sul PC in modalità locale, e in modalità cloud vanno al provider scelto dall'utente con la sua chiave. La telemetria non contiene testo o audio per regola di progetto.
- Valutazione tecnica, non parere legale: oggi non serve un programma HIPAA. Non costruire nulla.
- Non scrivere "HIPAA compliant" su sito o app. Se arriva un cliente sanitario USA, consultare un legale. Il BAA con un provider cloud resta tra quel cliente e il provider.

## Topic 4: mini design system dentro Fluent (proposta)

- Non costruire un design system da zero. Usare Fluent 2 e aggiungere un solo `ResourceDictionary` con scala dei testi, spaziature, stile delle card, colori degli speaker.
- Dato: `MeetingWindow.xaml` ha 99 `FontSize` scritti a mano e 0 stili di testo condivisi. In `Dimmy.Windows` non ho trovato cartelle `Themes`, `Styles` o `Resources`.
- Riferimenti: WinUI Gallery per i controlli, dashboard di PowerToys per la struttura della Home, `SettingsCard` di Windows Community Toolkit (è una nuova dipendenza, decidere prima).
- Ordine: finestra Meeting come primo caso, poi Home, poi le altre pagine solo se il risultato convince.
- Home: esiste già (`HomePanel`, voce "Home"), con card di benvenuto e statistiche. Da rifare: stato (motore, microfono, shortcut), ultime dettature, avvio meeting, checklist dei passi opzionali. Sostituire l'icona microfono con il logo vero.
- Mac: stessa idea nella stessa fase, per la parità tra sistemi.

## Decisioni aperte

- Design system: approvare il mini strato sopra Fluent, partendo dalla finestra Meeting?
- Gesti: approvare doppio tocco ed Esc come descritto? Consigliato sì, come prima cosa.
- Motore di prova: quale motore deve funzionare al primo avvio? Consigliato un piccolo modello locale con download in background, se i test sul primo avvio confermano che è abbastanza veloce.
- Stile: identità propria solo in onboarding e Home, dentro WinUI 3 e SwiftUI.

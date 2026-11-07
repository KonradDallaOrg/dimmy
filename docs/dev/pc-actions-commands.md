# PC commands: what Dimmy can do, and where the list comes from

EXPERIMENT, branch `exp/pc-actions`. Ctrl+Alt+X: the spoken words drive the PC
instead of being typed. Engine: local router (multilingual-e5-small + examples,
~3 ms) first, Claude Haiku when it is unsure; a live partial runs the command
while the user is still talking when it is "sure + stable" and harmless.

Status: ✅ done (local router and Haiku) · 🟡 Haiku only · ⬜ to do · ⛔ excluded
Live: ⚡ may run while you talk · ⏸ waits for the stop

## Our actions

| Action | Example (it / en) | Status | Live |
|---|---|---|---|
| open | "apri Teams" / "open Outlook" | ✅ | ⚡ |
| focus | "porta in primo piano Notepad" | 🟡 | ⚡ |
| place left / right / full / other monitor | "metti Chrome a destra", "porta Teams sull'altro monitor" | ✅ | ⚡ |
| tile two windows | "affianca Teams e Outlook" | ✅ | ⚡ |
| minimize / restore / show desktop | "nascondi Notepad", "ripristina Notepad", "mostra il desktop" | ✅ / 🟡 / 🟡 | ⚡ |
| find files by words and date | "trova i file di ieri" | ✅ | ⚡ |
| web: open or search a site | "naviga su YouTube", "cerca su Google il meteo" | ✅ | ⚡ |
| browser keys | "nuova scheda", "torna indietro", "ricarica" | ✅ | ⚡ (close tab ⏸) |
| media keys | "metti in pausa", "prossima canzone", "alza il volume" | ✅ | ⚡ |
| folders, latest download (selected, never opened) | "apri la cartella download", "l'ultimo file scaricato" | ✅ | ⚡ |
| Windows Settings pages | "impostazioni bluetooth" | ✅ | ⚡ |
| screenshot (snip) | "fai uno screenshot" | 🟡 | ⚡ |
| editing keys | "seleziona tutto", "annulla", "vai in fondo", "salva" | 🟡 | ⏸ |
| write at the end of a document | "scrivi in fondo a Notepad ciao" | 🟡 | ⏸ |

## From Atticus (MIT, SaarthurR/atticus-mac and FindlayHi/atticus-windows)

Fast path (no AI, English regex) and the Windows "hands" (`winctl.py`).

| Atticus | Ours |
|---|---|
| open / launch / start / pull up / bring up / focus / switch to / go to `<app>` | open, focus ✅ |
| open `<app>` on the left / right / full screen / laptop / monitor | open + place ✅ |
| snap / move / put / throw / send `<it or app>` … | place ✅ |
| search / google / look up `<x>` (on YouTube / Spotify / Amazon) | web ✅ (Spotify ⬜) |
| a URL or a file path said aloud | web ✅ · open a file ⬜ |
| play `<playlist>` (Spotify), add to queue | ⬜ (needs the Spotify app/API) |
| close / quit `<app>` (Mac) | ⬜ with confirmation (unsaved work) |
| media play / pause / next | media ✅ |
| clipboard read/write, paste into an app | type_at_end 🟡 |
| screenshots, read the screen | screenshot 🟡 · read the screen ⬜ |
| find files by name or by content (Windows Search) | find files ✅ (by content ⬜) |
| UI Automation: see / click / set a control by name | ⬜ with confirmation for "Send", "Delete", "Buy" |
| trash a file | ⛔ |
| learned shortcuts, "remember how to…", watch/routines, integrations | ⬜ later, not in this experiment |

## From Windows voice access (Microsoft's command list, used as inspiration)

| Voice access | Ours |
|---|---|
| Open / Start / Switch to / Go to `<app>` | ✅ |
| Close `<app>` / Close window | ⬜ with confirmation |
| Minimize / Maximize / Restore window | ✅ |
| Snap window to `<direction>` | ✅ |
| Go to desktop | ✅ |
| Show task switcher / List all windows | ⬜ |
| Search for `<x>` / Search on `<engine>` for `<x>` | ✅ |
| Press `<key>` / `<key1> plus <key2>` | ⬜ (a generic "press" step) |
| Click / Double-click / Right click `<item>` | ⬜ (UI Automation, with confirmation) |
| Scroll up / down, start / stop scrolling | ⬜ |
| Mouse grid, move mouse | ⬜ probably not: voice access already does it |
| Select all / that / word / line, copy, cut, paste, undo, redo | editing keys 🟡 (partial) |
| New line / Go to top / Go to bottom | editing keys 🟡 |
| Bold / italic / capitalize that | ⬜ |

## The user's own phrasings (from the logs, 2026-11-07)

These are the examples the local router needs most: conversational, Italian,
sometimes misheard.

| Said | What ran | Engine |
|---|---|---|
| "Apri Notepad" | open notepad | e5 (shadow: sure+stable 7 s before the stop) |
| "Apri il Notepad più e affiancalo a destra" | focus + place right | Haiku |
| "Apri nodpad affiancalo a sinistra e a destra ci affianchi Outlook" | open + tile | Haiku (shadow: plain "sure" would have fired too early on "Apri no") |
| "Dentro Notepad vai in fondo e vai a capo di due righe" | type_at_end "\n\n" | Haiku |
| "Ora puoi scriverci un Ciao" | type_at_end "Ciao" | Haiku (used the previous context) |
| "Puoi aprirmi Teams?" | open Teams | Haiku (e5 margin 0.002) |
| "A bassa note" / "Nascondi notepad" | nothing / moved to the other monitor | Haiku, before minimize existed |
| "Apri Chrom apri Chrome" | open chrome | Haiku (two spellings: now one app) |
| "Apri una nuova scheda" / "Apri una nuova tab" | focus chrome only | Haiku, before browser keys existed |
| "Navika su YouTube" | typed a URL into the page | Haiku, before web existed |

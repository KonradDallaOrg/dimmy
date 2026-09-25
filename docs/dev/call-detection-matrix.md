# Call detection — the two decision matrices

Written 2026-09-25 after an evening in which the same feature was broken
and repaired four times. Every row below is a test, and every number came
off a real log on this machine, not from reasoning about what ought to
happen.

Two decisions matter, and they fail in opposite directions:

- **Stopping** a recording that is running. The expensive mistake: one
  conversation becomes two files, and the second starts from silence.
- **Offering** to record. The cheap mistake: an extra popup annoys; a
  missing one costs a meeting.

## Matrix 1 — should this recording stop?

Implemented in `platforms/windows/Dimmy.Windows/Services/CallOriginVerdict.cs`,
tested in `CallOriginVerdictTests.cs`. The macOS equivalent is
`CallAudioWatch` in `Services/CallDetectionManager.swift`, which reaches the
same four outcomes from per-process CoreAudio facts.

| # | sessions of the call | someone else on the mic | devices moving | verdict |
|---|---|---|---|---|
| 1 | one is **Active** | — | — | on the call |
| 2 | process **dead** | — | — | **ENDED** |
| 3 | all Inactive | no | no | **ENDED** — hung up |
| 4 | all Inactive | **yes** | — | on the call — it moved device |
| 5 | all Inactive | no | **yes** | on the call — mid-move |
| 6 | none at all | — | yes / no | on the call / conclude nothing |

Why every input is needed:

- **An idle session means nothing on its own.** A hangup and a device move
  read identically. Measured 21:57, 22:11 and 22:37: the recording stopped
  0.9 to 1.9 s after the audio devices moved, and a second one started.
- **The device set cannot be compared against the one the call started on.**
  Turn a headset on and off and you are back where you began, which looks
  settled while a device just moved. That is why the move is *remembered*
  and forgotten only when the call is seen active again.
- **Row 5 exists for one measured instant.** At 23:13:48, mid-move on a
  browser call, Windows listed *nobody at all* as holding the microphone —
  not even the app on the call. Believing either signal alone splits the
  recording there.
- **Row 2 outranks everything.** Quitting the app during a device change is
  still quitting the app.

The only clock left is a 60 s backstop under row 6, so a recording can
never run for ever on a process that vanished and never came back. It
decides nothing that happens normally.

## Matrix 2 — should we offer to record?

Implemented in `core/src/call_detector.rs`, tested there (`nudge_row*`).
Rows 4 and 5 differ only in what the host does with the same outcome.

| # | which call | detection | auto-record | meeting active | already offered | excluded / cooldown | outcome |
|---|---|---|---|---|---|---|---|
| 1 | any | **off** | — | — | — | — | nothing |
| 2 | any | on | — | **yes** | — | — | nothing |
| 3 | any | on | — | no | — | **yes** | nothing |
| 4 | we saw it start | on | **on** | no | no | no | **record now** |
| 5 | we saw it start | on | **off** | no | no | no | **ask** |
| 6 | already under way | on | on **or** off | no | no | no | **ask, never take** |
| 7 | any | on | — | no | **yes** | — | nothing |
| 8 | it ended | — | — | — | — | — | forget the offer |

Row 6 is why `call_detected_preexisting` is a separate event rather than a
flag on `call_detected`: a host that does not know the case ignores an
unknown event, where an unread flag would have it record a call it must not
touch — one whose first half is gone and whose participants were never told.

Row 7 was broken until 23:39, when starting Dimmy inside a call offered it
twice, 21 s apart; the second popup answered a question the user had
already answered. The offer had been riding on `detection_emitted`, which is
cleared on every inactive reading so the NEXT call can be detected. Right
there, wrong here: an offer is made once and forgotten only at row 8.

## What this does not cover

Neither matrix says anything about the UI staying responsive, or a popup
actually appearing on screen. Those are checked by hand.

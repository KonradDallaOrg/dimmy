//! Recording-consent notices for meeting capture.
//!
//! Dimmy captures system audio during meetings, which means it records OTHER
//! people. Many jurisdictions (the EU under GDPR, US all-party-consent states)
//! require informed consent before recording a conversation. This module is
//! the single, cross-platform source of:
//!   - the text shown in the pre-recording confirmation modal (to the user),
//!   - the announcement spoken via the host's text-to-speech and pasted into
//!     the call chat (to the participants), worded accurately for the user's
//!     local-vs-cloud configuration, and
//!   - an append-only local audit log of consent events.
//!
//! The host decides WHEN to surface this (on meeting start, gated on the call
//! detector seeing a real call); this module owns WHAT it says and records
//! THAT it happened. Notices ship in Dimmy's six UI languages with an English
//! fallback for anything else.

/// Collapse a BCP-47-ish tag ("en-US", "it_IT") to one of the supported base
/// languages, falling back to English.
///
/// `pub(crate)` so the AI-Act notice in `meeting.rs` collapses languages the
/// same way instead of growing a second, drifting copy.
pub(crate) fn norm_lang(lang: &str) -> &'static str {
    let base = lang.to_ascii_lowercase();
    let base = base.split(['-', '_']).next().unwrap_or("en");
    match base {
        "it" => "it",
        "es" => "es",
        "fr" => "fr",
        "de" => "de",
        "pt" => "pt",
        _ => "en",
    }
}

/// Pre-recording confirmation shown to the RECORDER. Clicking through it is the
/// affirmation that consent was obtained (mirrors Notion's "Start = you
/// confirm everyone consented").
pub fn modal_text(lang: &str) -> String {
    match norm_lang(lang) {
        "it" => "Stai per registrare audio che puo' includere altre persone. Conferma di aver informato tutti i partecipanti e di averne ottenuto il consenso. In alcune aree (UE e diversi stati USA) e' obbligatorio per legge.",
        "es" => "Estas a punto de grabar audio que puede incluir a otras personas. Confirma que has informado a todos los participantes y obtenido su consentimiento. En algunas regiones (la UE y varios estados de EE. UU.) es obligatorio por ley.",
        "fr" => "Vous etes sur le point d'enregistrer de l'audio pouvant inclure d'autres personnes. Confirmez que vous avez informe tous les participants et obtenu leur consentement. Dans certaines regions (l'UE et plusieurs Etats americains), c'est obligatoire.",
        "de" => "Sie sind dabei, Audio aufzunehmen, das andere Personen enthalten kann. Bestaetigen Sie, dass Sie alle Teilnehmer informiert und ihre Zustimmung eingeholt haben. In einigen Regionen (der EU und mehreren US-Bundesstaaten) ist dies gesetzlich vorgeschrieben.",
        "pt" => "Voce esta prestes a gravar audio que pode incluir outras pessoas. Confirme que informou todos os participantes e obteve o consentimento deles. Em algumas regioes (a UE e varios estados dos EUA) isso e exigido por lei.",
        _ => "You are about to record audio that may include other people. Confirm you have informed all participants and obtained their consent. In some regions (the EU and several US states) this is required by law.",
    }
    .to_string()
}

/// Announcement to the PARTICIPANTS, used for the spoken (TTS) notice and the
/// chat message. The second sentence is accurate to the user's configuration:
/// local-only recording vs cloud processing (the audio leaving the device is a
/// material GDPR fact that the notice must disclose).
pub fn announcement_text(lang: &str, cloud_processing: bool) -> String {
    announcement_variant(lang, cloud_processing, 0)
}

/// How many wordings of the announcement exist per language and per storage
/// mode. Variant 0 is the original wording and must stay byte-identical: it
/// is what shipped, and the chat message is a legal notice, not copy.
pub const ANNOUNCEMENT_VARIANTS: usize = 3;

/// One wording of the participant announcement. Same two obligations in all
/// of them (a recording is happening, and where the audio goes); only the
/// phrasing changes, so the same notice does not sound like a recording
/// announcing itself the twentieth time.
///
/// `variant` is taken modulo [`ANNOUNCEMENT_VARIANTS`], so a host that has
/// drifted out of range gets a valid notice rather than none.
pub fn announcement_variant(lang: &str, cloud_processing: bool, variant: usize) -> String {
    let l = norm_lang(lang);
    let v = variant % ANNOUNCEMENT_VARIANTS;
    // ASCII-only, like the rest of this module: the same string is spoken,
    // pasted into the chat and recorded as audio, and one spelling for all
    // three is worth more than the accents.
    let base = match (l, v) {
        ("it", 0) => "Avviso: questo meeting viene registrato e trascritto per prendere appunti.",
        ("it", 1) => "Vi dico solo che sto registrando il meeting, mi serve per gli appunti.",
        ("it", _) => "Piccola cosa: registro il meeting, in modo da non perdermi niente.",
        ("es", 0) => "Aviso: esta reunion se esta grabando y transcribiendo para tomar notas.",
        ("es", 1) => "Solo para decirlo: estoy grabando la reunion para tomar notas.",
        ("es", _) => "Una cosa rapida: grabo la reunion para no perderme nada.",
        ("fr", 0) => {
            "Information : cette reunion est enregistree et transcrite pour prendre des notes."
        }
        ("fr", 1) => "Je vous le dis simplement : j'enregistre la reunion pour prendre des notes.",
        ("fr", _) => "Petite chose : j'enregistre la reunion pour ne rien rater.",
        ("de", 0) => "Hinweis: Dieses Meeting wird aufgezeichnet und fuer Notizen transkribiert.",
        ("de", 1) => "Nur damit ihr es wisst: Ich nehme das Meeting auf, fuer meine Notizen.",
        ("de", _) => "Eine Kleinigkeit: Ich nehme das Meeting auf, damit mir nichts entgeht.",
        ("pt", 0) => "Aviso: esta reuniao esta sendo gravada e transcrita para anotacoes.",
        ("pt", 1) => "So para avisar: estou gravando a reuniao para fazer anotacoes.",
        ("pt", _) => "Uma coisa rapida: estou gravando a reuniao para nao perder nada.",
        (_, 0) => "Quick note: this meeting is being recorded and transcribed for note-taking.",
        (_, 1) => "Just so you know, I'm recording the meeting so I can take notes.",
        (_, _) => "One small thing: I'm recording the meeting so I don't miss anything.",
    };
    let storage = if cloud_processing {
        match (l, v) {
            ("it", 0) => "L'audio viene elaborato da un servizio esterno per generare gli appunti.",
            ("it", 1) => "L'audio passa da un servizio esterno che scrive gli appunti.",
            ("it", _) => "Per gli appunti l'audio viene elaborato da un servizio esterno.",
            ("es", 0) => "El audio se procesa con un servicio externo para generar las notas.",
            ("es", 1) => "El audio pasa por un servicio externo que escribe las notas.",
            ("es", _) => "Para las notas, el audio se procesa con un servicio externo.",
            ("fr", 0) => "L'audio est traite par un service externe pour generer les notes.",
            ("fr", 1) => "L'audio passe par un service externe qui redige les notes.",
            ("fr", _) => "Pour les notes, l'audio est traite par un service externe.",
            ("de", 0) => {
                "Das Audio wird von einem externen Dienst verarbeitet, um die Notizen zu erstellen."
            }
            ("de", 1) => "Das Audio laeuft ueber einen externen Dienst, der die Notizen schreibt.",
            ("de", _) => "Fuer die Notizen wird das Audio von einem externen Dienst verarbeitet.",
            ("pt", 0) => "O audio e processado por um servico externo para gerar as anotacoes.",
            ("pt", 1) => "O audio passa por um servico externo que escreve as anotacoes.",
            ("pt", _) => "Para as anotacoes, o audio e processado por um servico externo.",
            (_, 0) => "The audio is processed by an external service to produce the notes.",
            (_, 1) => "The audio goes through an external service that writes the notes.",
            (_, _) => "For the notes, the audio is processed by an external service.",
        }
    } else {
        match (l, v) {
            ("it", 0) => "La registrazione resta sul mio dispositivo.",
            ("it", 1) => "L'audio non esce dal mio dispositivo.",
            ("it", _) => "La registrazione resta qui sul mio dispositivo.",
            ("es", 0) => "La grabacion permanece en mi dispositivo.",
            ("es", 1) => "La grabacion no sale de mi dispositivo.",
            ("es", _) => "La grabacion se queda aqui en mi dispositivo.",
            ("fr", 0) => "L'enregistrement reste sur mon appareil.",
            ("fr", 1) => "L'enregistrement ne quitte pas mon appareil.",
            ("fr", _) => "L'enregistrement reste ici sur mon appareil.",
            ("de", 0) => "Die Aufnahme bleibt auf meinem Geraet.",
            ("de", 1) => "Die Aufnahme verlaesst mein Geraet nicht.",
            ("de", _) => "Die Aufnahme bleibt hier auf meinem Geraet.",
            ("pt", 0) => "A gravacao permanece no meu dispositivo.",
            ("pt", 1) => "A gravacao nao sai do meu dispositivo.",
            ("pt", _) => "A gravacao fica aqui no meu dispositivo.",
            (_, 0) => "The recording stays on my device.",
            (_, 1) => "The recording never leaves my device.",
            (_, _) => "The recording stays here on my device.",
        }
    };
    format!("{base} {storage}")
}

/// The recorded announcement for `(lang, cloud_processing, variant)`, or the
/// English one when the language is unsupported.
///
/// The audio is compiled into the library rather than shipped as loose files:
/// one delivery path for all three hosts, and a missing take becomes a build
/// error instead of a meeting that announces nothing.
pub fn announcement_audio(lang: &str, cloud_processing: bool, variant: usize) -> &'static [u8] {
    macro_rules! takes {
        ($lang:literal, $mode:literal) => {
            [
                include_bytes!(concat!(
                    "../assets/consent/consent-",
                    $lang,
                    "-",
                    $mode,
                    "-0.mp3"
                )) as &[u8],
                include_bytes!(concat!(
                    "../assets/consent/consent-",
                    $lang,
                    "-",
                    $mode,
                    "-1.mp3"
                )) as &[u8],
                include_bytes!(concat!(
                    "../assets/consent/consent-",
                    $lang,
                    "-",
                    $mode,
                    "-2.mp3"
                )) as &[u8],
            ]
        };
    }
    let v = variant % ANNOUNCEMENT_VARIANTS;
    let takes = match (norm_lang(lang), cloud_processing) {
        ("it", false) => takes!("it", "local"),
        ("it", true) => takes!("it", "cloud"),
        ("es", false) => takes!("es", "local"),
        ("es", true) => takes!("es", "cloud"),
        ("fr", false) => takes!("fr", "local"),
        ("fr", true) => takes!("fr", "cloud"),
        ("de", false) => takes!("de", "local"),
        ("de", true) => takes!("de", "cloud"),
        ("pt", false) => takes!("pt", "local"),
        ("pt", true) => takes!("pt", "cloud"),
        (_, false) => takes!("en", "local"),
        (_, true) => takes!("en", "cloud"),
    };
    let audio = takes[v];
    assert!(!audio.is_empty(), "consent take must not be empty");
    audio
}

/// Which wording to use this time. Rotating is not decoration: the notice is
/// spoken to the same colleagues every day, and one that is word-for-word
/// identical every time stops being heard.
pub fn pick_announcement_variant(seed: u64) -> usize {
    (seed % ANNOUNCEMENT_VARIANTS as u64) as usize
}

/// Localized UI chrome for the recording-consent dialog (title, the helper
/// line above the announcement, and the two button labels). Kept here so every
/// platform renders the SAME wording instead of hardcoding English host-side
/// (which left the buttons + title in English while the body was localized).
/// ASCII-apostrophe style matches `modal_text`. Returns `None` for unknown
/// kinds so the FFI can reject them.
pub fn ui_text(kind: &str, lang: &str) -> Option<String> {
    let l = norm_lang(lang);
    let s = match kind {
        "title" => match l {
            "it" => "Avviso di registrazione",
            "es" => "Aviso de grabacion",
            "fr" => "Avis d'enregistrement",
            "de" => "Aufnahmehinweis",
            "pt" => "Aviso de gravacao",
            _ => "Recording notice",
        },
        "intro" => match l {
            "it" => "Dimmy leggera' questo avviso ad alta voce e lo copiera' cosi' puoi incollarlo nella chat del meeting:",
            "es" => "Dimmy leera este aviso en voz alta y lo copiara para que puedas pegarlo en el chat de la reunion:",
            "fr" => "Dimmy lira cet avis a voix haute et le copiera pour que vous puissiez le coller dans le chat de la reunion :",
            "de" => "Dimmy liest diesen Hinweis vor und kopiert ihn, damit Sie ihn in den Meeting-Chat einfuegen koennen:",
            "pt" => "O Dimmy lera este aviso em voz alta e o copiara para que voce possa cola-lo no chat da reuniao:",
            _ => "Dimmy will read this notice aloud and copy it so you can paste it in the meeting chat:",
        },
        "confirm" => match l {
            "it" => "Ho il consenso, avvia",
            "es" => "Tengo consentimiento, iniciar",
            "fr" => "J'ai le consentement, demarrer",
            "de" => "Zustimmung liegt vor, starten",
            "pt" => "Tenho consentimento, iniciar",
            _ => "I have consent, start",
        },
        "cancel" => match l {
            "it" => "Annulla",
            "es" => "Cancelar",
            "fr" => "Annuler",
            "de" => "Abbrechen",
            "pt" => "Cancelar",
            _ => "Cancel",
        },
        _ => return None,
    };
    Some(s.to_string())
}

/// JSON line for one consent event. Pure (no clock / IO) so it can be tested.
/// `kind` is the event tag the host emits: "confirmed", "declined",
/// "announced", "chat_copied".
pub fn format_event(kind: &str, lang: &str, now_epoch: i64) -> String {
    serde_json::json!({
        "ts": now_epoch,
        "event": kind,
        "lang": lang,
    })
    .to_string()
}

/// Append a consent event to `<config_dir>/consent.jsonl` (append-only audit
/// trail). Best-effort: a failure to write must never block recording.
pub fn append_event(kind: &str, lang: &str) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let line = format_event(kind, lang, now);
    if let Some(dir) = crate::config_dir_path() {
        let _ = std::fs::create_dir_all(&dir);
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("consent.jsonl"))
        {
            Ok(mut f) => {
                use std::io::Write;
                if let Err(e) = writeln!(f, "{line}") {
                    // Still best-effort (never block recording), but an
                    // incomplete AUDIT trail must at least be visible in
                    // the log (audit 2026-07-02: no silent failures).
                    crate::log(&format!("[Consent] WARN audit write failed: {e}"));
                }
            }
            Err(e) => {
                crate::log(&format!("[Consent] WARN audit open failed: {e}"));
            }
        }
    } else {
        crate::log("[Consent] WARN audit skipped: config dir unavailable");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modal_text_localizes_and_falls_back_to_english() {
        assert!(modal_text("it").contains("consenso"));
        assert!(modal_text("de").contains("Zustimmung"));
        // Unknown / regional tags fall back to English.
        assert!(modal_text("ja").contains("consent"));
        assert!(modal_text("en-US").contains("consent"));
        assert!(!modal_text("it").is_empty());
    }

    #[test]
    fn announcement_discloses_local_vs_cloud() {
        let it_local = announcement_text("it", false);
        let it_cloud = announcement_text("it", true);
        assert!(it_local.contains("dispositivo"));
        assert!(it_cloud.contains("servizio esterno"));
        assert_ne!(it_local, it_cloud);
        // English fallback keeps the same distinction.
        assert!(announcement_text("xx", false).contains("on my device"));
        assert!(announcement_text("xx", true).contains("external service"));
    }

    #[test]
    fn ui_text_localizes_chrome_and_rejects_unknown() {
        assert_eq!(ui_text("title", "it").unwrap(), "Avviso di registrazione");
        assert_eq!(ui_text("cancel", "de").unwrap(), "Abbrechen");
        assert!(ui_text("confirm", "it").unwrap().contains("consenso"));
        assert!(ui_text("intro", "fr").unwrap().contains("Dimmy"));
        // Unknown language → English fallback; unknown kind → None.
        assert_eq!(ui_text("title", "ja").unwrap(), "Recording notice");
        assert!(ui_text("bogus", "it").is_none());
    }

    #[test]
    fn format_event_is_valid_json_with_fields() {
        let line = format_event("confirmed", "it", 1_700_000_000);
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["event"], "confirmed");
        assert_eq!(v["lang"], "it");
        assert_eq!(v["ts"], 1_700_000_000_i64);
    }

    /// The recorded take and the text pasted into the chat must say the SAME
    /// thing. They are produced by different tools months apart, so nothing
    /// but this test keeps them together: change a word in
    /// `announcement_variant` and the build goes red until the line is
    /// re-recorded.
    #[test]
    fn every_recorded_take_says_exactly_what_the_notice_says() {
        macro_rules! spoken {
            ($lang:literal, $mode:literal) => {
                [
                    include_str!(concat!(
                        "../assets/consent/consent-",
                        $lang,
                        "-",
                        $mode,
                        "-0.txt"
                    )),
                    include_str!(concat!(
                        "../assets/consent/consent-",
                        $lang,
                        "-",
                        $mode,
                        "-1.txt"
                    )),
                    include_str!(concat!(
                        "../assets/consent/consent-",
                        $lang,
                        "-",
                        $mode,
                        "-2.txt"
                    )),
                ]
            };
        }
        let table: [(&str, bool, [&str; 3]); 12] = [
            ("it", false, spoken!("it", "local")),
            ("it", true, spoken!("it", "cloud")),
            ("es", false, spoken!("es", "local")),
            ("es", true, spoken!("es", "cloud")),
            ("fr", false, spoken!("fr", "local")),
            ("fr", true, spoken!("fr", "cloud")),
            ("de", false, spoken!("de", "local")),
            ("de", true, spoken!("de", "cloud")),
            ("pt", false, spoken!("pt", "local")),
            ("pt", true, spoken!("pt", "cloud")),
            ("en", false, spoken!("en", "local")),
            ("en", true, spoken!("en", "cloud")),
        ];
        for (lang, cloud, takes) in table {
            for (v, recorded) in takes.iter().enumerate() {
                assert_eq!(
                    recorded.trim(),
                    announcement_variant(lang, cloud, v),
                    "{lang}/{}/{v}: the recording and the notice have drifted",
                    if cloud { "cloud" } else { "local" }
                );
                assert!(
                    !announcement_audio(lang, cloud, v).is_empty(),
                    "{lang}/{v}: missing audio"
                );
            }
        }
    }

    #[test]
    fn variant_picking_stays_in_range_and_rotates() {
        let picked: Vec<usize> = (0..6).map(pick_announcement_variant).collect();
        assert_eq!(picked, vec![0, 1, 2, 0, 1, 2]);
        for seed in [0u64, 7, u64::MAX] {
            assert!(pick_announcement_variant(seed) < ANNOUNCEMENT_VARIANTS);
        }
    }

    /// Variant 0 is what shipped. A notice people already heard must not
    /// change wording because a new take was added next to it.
    #[test]
    fn variant_zero_is_the_wording_that_shipped() {
        assert_eq!(
            announcement_variant("it", false, 0),
            "Avviso: questo meeting viene registrato e trascritto per prendere appunti. \
             La registrazione resta sul mio dispositivo."
        );
        assert_eq!(
            announcement_text("it", false),
            announcement_variant("it", false, 0)
        );
    }
}

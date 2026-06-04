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
fn norm_lang(lang: &str) -> &'static str {
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
    let l = norm_lang(lang);
    let base = match l {
        "it" => "Avviso: questo meeting viene registrato e trascritto per prendere appunti.",
        "es" => "Aviso: esta reunion se esta grabando y transcribiendo para tomar notas.",
        "fr" => "Information : cette reunion est enregistree et transcrite pour prendre des notes.",
        "de" => "Hinweis: Dieses Meeting wird aufgezeichnet und fuer Notizen transkribiert.",
        "pt" => "Aviso: esta reuniao esta sendo gravada e transcrita para anotacoes.",
        _ => "Quick note: this meeting is being recorded and transcribed for note-taking.",
    };
    let storage = if cloud_processing {
        match l {
            "it" => "L'audio viene elaborato da un servizio esterno per generare gli appunti.",
            "es" => "El audio se procesa con un servicio externo para generar las notas.",
            "fr" => "L'audio est traite par un service externe pour generer les notes.",
            "de" => {
                "Das Audio wird von einem externen Dienst verarbeitet, um die Notizen zu erstellen."
            }
            "pt" => "O audio e processado por um servico externo para gerar as anotacoes.",
            _ => "The audio is processed by an external service to produce the notes.",
        }
    } else {
        match l {
            "it" => "La registrazione resta sul mio dispositivo.",
            "es" => "La grabacion permanece en mi dispositivo.",
            "fr" => "L'enregistrement reste sur mon appareil.",
            "de" => "Die Aufnahme bleibt auf meinem Geraet.",
            "pt" => "A gravacao permanece no meu dispositivo.",
            _ => "The recording stays on my device.",
        }
    };
    format!("{base} {storage}")
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
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("consent.jsonl"))
        {
            use std::io::Write;
            let _ = writeln!(f, "{line}");
        }
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
    fn format_event_is_valid_json_with_fields() {
        let line = format_event("confirmed", "it", 1_700_000_000);
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["event"], "confirmed");
        assert_eq!(v["lang"], "it");
        assert_eq!(v["ts"], 1_700_000_000_i64);
    }
}

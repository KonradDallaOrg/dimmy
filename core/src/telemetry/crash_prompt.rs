//! What happens to a native crash from the previous launch (an abort marker,
//! a minidump, or both).
//!
//! - Crash reports on: sent silently, as before.
//! - Crash reports off or not answered: kept, and the host asks ONCE
//!   ("Dimmy closed unexpectedly last time…"). "Send" sends only this report;
//!   "Don't send" deletes it. The app closing before an answer counts as
//!   "Don't send": the dumps are marked asked and the next launch deletes them.
//!
//! The hosts only show the dialog; this module owns the state and the
//! wording, so Windows and macOS behave and read the same.

use super::crash_dump::{self, Dump};
use std::sync::Mutex;

#[derive(Default)]
struct Pending {
    abort: Option<String>,
    dumps: Vec<Dump>,
    asked: bool,
}

impl Pending {
    fn is_empty(&self) -> bool {
        self.abort.is_none() && self.dumps.is_empty()
    }
}

static PENDING: Mutex<Option<Pending>> = Mutex::new(None);

fn lock() -> std::sync::MutexGuard<'static, Option<Pending>> {
    PENDING.lock().unwrap_or_else(|p| p.into_inner())
}

/// Called once the saved consent is known, with what the last launch left.
pub fn on_launch(abort: Option<String>, dumps: Vec<Dump>, consented: bool) {
    if consented {
        send(abort, dumps);
        return;
    }
    let pending = Pending {
        abort,
        dumps,
        asked: false,
    };
    if !pending.is_empty() {
        crate::log(&format!(
            "[telemetry] crash report kept until the user answers ({} dump(s))",
            pending.dumps.len()
        ));
        *lock() = Some(pending);
    }
}

fn send(abort: Option<String>, dumps: Vec<Dump>) {
    if dumps.is_empty() {
        if let Some(abort) = abort {
            super::report(
                super::Subsystem::Native,
                "abort",
                "",
                &super::report::slug(&abort),
            );
        }
        return;
    }
    // On Windows an abort leaves a dump as well: one event, the dump's stack
    // plus the marker's free-memory line.
    for (i, dump) in dumps.iter().enumerate() {
        let detail = if i == 0 { abort.as_deref() } else { None };
        super::sentry_pipeline::send_native_crash(
            &dump.release,
            &dump.environment,
            detail,
            Some(&dump.path),
        );
    }
}

/// True once per crash: the host shows the dialog now. Never when nothing
/// could be sent (no DSN in this build).
pub fn take_prompt() -> bool {
    take_prompt_if(super::has_compiled_dsn())
}

fn take_prompt_if(can_send: bool) -> bool {
    let mut slot = lock();
    let Some(pending) = slot.as_mut() else {
        return false;
    };
    if !can_send || pending.asked || pending.is_empty() {
        return false;
    }
    pending.asked = true;
    pending.dumps = pending.dumps.iter().map(crash_dump::mark_asked).collect();
    true
}

/// The user's answer. `1` sent, `0` deleted, `-1` nothing was pending.
/// "Send" goes out even with crash reports off: it is this one report, on
/// an explicit request.
pub fn resolve(send_it: bool) -> i32 {
    let Some(pending) = lock().take() else {
        return -1;
    };
    if pending.is_empty() {
        return -1;
    }
    if !send_it {
        for dump in &pending.dumps {
            let _ = std::fs::remove_file(&dump.path);
        }
        return 0;
    }
    if pending.dumps.is_empty() {
        super::sentry_pipeline::send_native_crash(
            &super::sentry_pipeline::current_release(),
            super::sentry_pipeline::current_environment(),
            pending.abort.as_deref(),
            None,
        );
    } else {
        send(pending.abort, pending.dumps);
    }
    1
}

/// The dialog's words in Dimmy's six UI languages, English for the rest.
/// Kinds: `title`, `body`, `send`, `dont_send`, `always`.
pub fn text(kind: &str, lang: &str) -> Option<&'static str> {
    let lang = crate::consent::norm_lang(lang);
    let t = match (kind, lang) {
        ("title", "it") => "Dimmy si è chiuso in modo inatteso",
        ("title", "es") => "Dimmy se cerró de forma inesperada",
        ("title", "fr") => "Dimmy s'est fermé de façon inattendue",
        ("title", "de") => "Dimmy wurde unerwartet beendet",
        ("title", "pt") => "O Dimmy fechou inesperadamente",
        ("title", _) => "Dimmy closed unexpectedly",
        ("body", "it") => "L'ultima volta Dimmy si è chiuso in modo inatteso. Vuoi inviare il rapporto sull'arresto così possiamo correggerlo?",
        ("body", "es") => "La última vez Dimmy se cerró de forma inesperada. ¿Envías el informe del fallo para que podamos corregirlo?",
        ("body", "fr") => "La dernière fois, Dimmy s'est fermé de façon inattendue. Envoyer le rapport de plantage pour que nous puissions le corriger ?",
        ("body", "de") => "Dimmy wurde beim letzten Mal unerwartet beendet. Absturzbericht senden, damit wir den Fehler beheben können?",
        ("body", "pt") => "Da última vez o Dimmy fechou inesperadamente. Enviar o relatório de falha para podermos corrigi-lo?",
        ("body", _) => "Dimmy closed unexpectedly last time. Send the crash report so we can fix it?",
        ("send", "it") => "Invia",
        ("send", "es") => "Enviar",
        ("send", "fr") => "Envoyer",
        ("send", "de") => "Senden",
        ("send", "pt") => "Enviar",
        ("send", _) => "Send",
        ("dont_send", "it") => "Non inviare",
        ("dont_send", "es") => "No enviar",
        ("dont_send", "fr") => "Ne pas envoyer",
        ("dont_send", "de") => "Nicht senden",
        ("dont_send", "pt") => "Não enviar",
        ("dont_send", _) => "Don't send",
        ("always", "it") => "Invia sempre i rapporti sugli arresti",
        ("always", "es") => "Enviar siempre los informes de fallos",
        ("always", "fr") => "Toujours envoyer les rapports de plantage",
        ("always", "de") => "Absturzberichte immer senden",
        ("always", "pt") => "Enviar sempre os relatórios de falha",
        ("always", _) => "Always send crash reports",
        _ => return None,
    };
    Some(t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    fn dump_in(tag: &str) -> (std::path::PathBuf, Dump) {
        let dir = std::env::temp_dir().join(format!("dimmy-prompt-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(crash_dump::dump_name(1, 1, "development", "dimmy@1.0.0"));
        std::fs::write(&path, b"MDMP").unwrap();
        let dump = crash_dump::collect_in(&dir, std::time::SystemTime::now()).remove(0);
        (dir, dump)
    }

    #[test]
    #[serial(crash_prompt)]
    fn the_prompt_shows_once_per_crash() {
        on_launch(Some("native abort".into()), Vec::new(), false);
        assert!(take_prompt_if(true));
        assert!(!take_prompt_if(true), "never twice");
        assert_eq!(resolve(false), 0);
        assert_eq!(resolve(false), -1, "consumed");
    }

    #[test]
    #[serial(crash_prompt)]
    fn nothing_to_ask_about_means_no_prompt() {
        *lock() = None;
        on_launch(None, Vec::new(), false);
        assert!(!take_prompt_if(true));
        assert_eq!(resolve(true), -1);
    }

    #[test]
    #[serial(crash_prompt)]
    fn a_build_that_cannot_send_does_not_ask() {
        on_launch(Some("native abort".into()), Vec::new(), false);
        assert!(!take_prompt_if(false));
        assert_eq!(resolve(false), 0);
    }

    #[test]
    #[serial(crash_prompt)]
    fn with_consent_nothing_waits_for_an_answer() {
        *lock() = None;
        on_launch(Some("native abort".into()), Vec::new(), true);
        assert!(!take_prompt_if(true));
    }

    #[test]
    #[serial(crash_prompt)]
    fn dont_send_deletes_the_dump() {
        let (dir, dump) = dump_in("dont");
        on_launch(None, vec![dump], false);
        assert!(take_prompt_if(true));
        let asked = lock().as_ref().unwrap().dumps[0].path.clone();
        assert!(
            asked.exists(),
            "renamed, not deleted, while the dialog is up"
        );
        assert_eq!(resolve(false), 0);
        assert!(!asked.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[serial(crash_prompt)]
    fn an_unanswered_prompt_is_gone_at_the_next_launch() {
        let (dir, dump) = dump_in("unanswered");
        on_launch(None, vec![dump], false);
        assert!(take_prompt_if(true));
        // The app closes here. Next launch:
        assert!(crash_dump::collect_in(&dir, std::time::SystemTime::now()).is_empty());
        *lock() = None;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_language_has_every_word() {
        for lang in ["en", "it", "es", "fr", "de", "pt", "ja"] {
            for kind in ["title", "body", "send", "dont_send", "always"] {
                assert!(
                    text(kind, lang).is_some_and(|t| !t.is_empty()),
                    "{kind}/{lang}"
                );
            }
        }
        assert_eq!(text("nope", "en"), None);
        assert_eq!(
            text("body", "en-US"),
            Some("Dimmy closed unexpectedly last time. Send the crash report so we can fix it?")
        );
    }
}

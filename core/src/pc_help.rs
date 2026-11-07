//! "Cosa posso dire?" — EXPERIMENT (branch `exp/pc-actions`).
//!
//! Spoken with the execute shortcut, it runs nothing: it answers with a few
//! examples per kind of command, using the apps that are open now and some
//! commands of the window in front, so the examples can be said as they are.

/// The words that ask for the list. Narrow on purpose: a real command that
/// happens to contain "comandi" must still run.
pub fn is_help_request(spoken: &str) -> bool {
    let s: String = spoken
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    const ASKS: &[&str] = &[
        "posso dire",
        "posso fare",
        "quali comandi",
        "che comandi",
        "what can i say",
        "what can i do",
    ];
    matches!(s.as_str(), "aiuto" | "help" | "comandi") || ASKS.iter().any(|a| s.contains(a))
}

/// The list, from the open apps and the front app's commands.
pub fn help_text(open_apps: &[String], front: Option<(&str, &[String])>) -> String {
    let mut names: Vec<&str> = Vec::new();
    for n in open_apps
        .iter()
        .map(String::as_str)
        .chain(["Blocco note", "Calcolatrice"])
    {
        if names.len() < 2 && !names.iter().any(|m| m.eq_ignore_ascii_case(n)) {
            names.push(n);
        }
    }
    let (a, b) = (names[0], names[1]);
    let mut t = format!(
        "• App e finestre: «apri {a}», «metti {a} a sinistra», «affianca {a} e {b}», «riduci a icona {b}»\n\
         • Web: «cerca ricette su YouTube», «apri GitHub», «nuova scheda», «indietro»\n\
         • Musica: «pausa», «prossima», «alza il volume»\n\
         • Cartelle e file: «apri Download», «l'ultimo file scaricato», «trova i file di ieri»\n\
         • Impostazioni: «impostazioni Bluetooth», «impostazioni Wi-Fi»"
    );
    if let Some((app, cmds)) = front.filter(|(_, c)| !c.is_empty()) {
        let said: Vec<String> = cmds.iter().map(|c| format!("«{c}»")).collect();
        t.push_str(&format!("\n• In {app}: {}", said.join(", ")));
    }
    assert!(!t.contains("«apri »"), "every example names an app");
    t
}

/// The list for the desktop as it is now.
pub fn help_now() -> String {
    let apps = crate::pc_control::open_app_names();
    let front = crate::pc_appcmds::examples(4);
    help_text(
        &apps,
        front.as_ref().map(|(p, c)| (p.as_str(), c.as_slice())),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_question_is_recognised_in_both_languages() {
        for s in [
            "cosa posso dire?",
            "Che cosa posso dire",
            "quali comandi posso usare",
            "cosa posso fare",
            "aiuto",
            "What can I say?",
            "help",
        ] {
            assert!(is_help_request(s), "{s}");
        }
    }

    #[test]
    fn a_real_command_is_not_the_question() {
        for s in [
            "apri Spotify",
            "cerca aiuto su Google",
            "apri il prompt dei comandi",
            "help me find the files of yesterday",
            "",
        ] {
            assert!(!is_help_request(s), "{s}");
        }
    }

    #[test]
    fn the_examples_use_the_apps_that_are_open() {
        let apps = vec!["Spotify".to_string(), "Google Chrome".to_string()];
        let t = help_text(&apps, None);
        assert!(t.contains("apri Spotify"), "{t}");
        assert!(t.contains("affianca Spotify e Google Chrome"), "{t}");
        for kind in ["Web", "Musica", "Cartelle", "Impostazioni"] {
            assert!(t.contains(kind), "{kind} missing: {t}");
        }
    }

    #[test]
    fn with_no_open_app_the_examples_still_have_names() {
        let t = help_text(&[], None);
        assert!(t.contains("apri Blocco note"), "{t}");
        assert!(!t.contains("«apri »"), "{t}");
    }

    #[test]
    fn the_front_app_commands_are_listed_under_its_name() {
        let cmds = vec!["Salva tutto".to_string(), "Trova".to_string()];
        let t = help_text(&[], Some(("Notepad++", &cmds)));
        assert!(t.contains("In Notepad++"), "{t}");
        assert!(t.contains("«Salva tutto»") && t.contains("«Trova»"), "{t}");
    }
}

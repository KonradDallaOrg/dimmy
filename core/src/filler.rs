use std::cmp::Reverse;

use regex::Regex;

/// Remove filler words from transcribed text for the given ISO 639-1 language code.
///
/// Supports: Italian (it), English (en), Spanish (es), French (fr), German (de), Portuguese (pt).
/// Unknown languages default to English. Empty text returns empty string.
pub fn remove_fillers(text: &str, language: &str) -> String {
    assert!(
        language.len() <= 10,
        "language code must be a short ISO 639-1 string"
    );

    if text.is_empty() {
        return String::new();
    }

    let fillers = fillers_for_language(language);
    let mut result = text.to_string();

    // Process fillers longest-first so multi-word fillers are removed before their
    // sub-parts (e.g. "you know" before "you").
    let mut sorted_fillers = fillers.to_vec();
    sorted_fillers.sort_by_key(|f| Reverse(f.len()));

    for filler in &sorted_fillers {
        let is_short = filler.len() <= 3 && !filler.contains(' ');
        if is_short {
            result = remove_short_filler(&result, filler);
        } else {
            let pattern = build_filler_pattern(filler);
            let re = Regex::new(&pattern).expect("filler regex must compile");
            result = re.replace_all(&result, "").to_string();
        }
    }

    result = clean_orphaned_punctuation(&result);
    result = collapse_spaces(&result);
    result = trim_leading_punctuation(&result);
    result = result.trim().to_string();

    // Postconditions
    assert!(
        !result.contains("  "),
        "result must not contain double spaces"
    );

    result
}

/// Return the filler word list for a given language code.
/// Unknown languages default to English.
fn fillers_for_language(lang: &str) -> &'static [&'static str] {
    match lang {
        "it" => &[
            "come dire",
            "praticamente",
            "insomma",
            "diciamo",
            "vabbè",
            "cioè",
            "tipo",
            "voglio dire",
            "in pratica",
            "ecco",
            "fondamentalmente",
            "ehm",
            "uhm",
            "eh",
        ],
        "es" => &[
            "o sea",
            "digamos",
            "bueno",
            "tipo",
            "este",
            "pues",
            "la verdad",
            "en plan",
            "ehm",
            "uhm",
        ],
        "fr" => &[
            "en fait", "du coup", "voilà", "genre", "enfin", "quoi", "bon", "ben", "bah", "euh",
        ],
        "de" => &[
            "sozusagen",
            "irgendwie",
            "eigentlich",
            "quasi",
            "also",
            "halt",
            "eben",
            "ja",
            "naja",
            "ähm",
            "äh",
        ],
        "pt" => &[
            "quer dizer",
            "digamos",
            "então",
            "tipo",
            "né",
            "bem",
            "bom",
            "olha",
            "eh",
            "ahm",
        ],
        // English is the default for "en" and any unknown language
        _ => &[
            "you know",
            "sort of",
            "kind of",
            "basically",
            "actually",
            "literally",
            "I mean",
            "right",
            "like",
            "well",
            "so",
            "um",
            "uh",
            "uhm",
            "ehm",
        ],
    }
}

/// Build a regex pattern for a longer filler word (> 3 chars or multi-word).
/// Uses standard `\b` word boundaries.
fn build_filler_pattern(filler: &str) -> String {
    let escaped = regex::escape(filler);
    format!(r"(?i)\b{}\b", escaped)
}

/// Remove a short filler word (<= 3 chars) with strict positional matching.
///
/// Short fillers like "so", "um", "eh" are common substrings in real words
/// (e.g. "someone", "umbrella", "vehicle"). We only remove them when they
/// appear as standalone fillers: at start of text, after punctuation, or
/// followed by punctuation/comma.
///
/// Rust's regex crate doesn't support look-around, so we capture both the
/// preceding and trailing context and put them back in the replacement.
fn remove_short_filler(text: &str, filler: &str) -> String {
    let escaped = regex::escape(filler);

    // Group 1 = preceding context (start-of-string or punctuation+space)
    // Group 2 = trailing context (punctuation or space or end)
    // The filler word plus optional surrounding whitespace is consumed and dropped.
    let pattern = format!(r"(?i)(^|[.!?;,]\s*)\s*{}\s*([,;.!?\s]|$)", escaped);
    let re = Regex::new(&pattern).expect("short filler regex must compile");
    re.replace_all(text, "$1$2").to_string()
}

/// Clean up orphaned punctuation artifacts left after filler removal.
/// Examples: ", ," -> ",", ",," -> ","
fn clean_orphaned_punctuation(text: &str) -> String {
    let mut result = text.to_string();

    // Remove sequences of commas with optional spaces between them: ", ," or ",,"
    let re_dup_commas = Regex::new(r",(\s*,)+").expect("regex must compile");
    result = re_dup_commas.replace_all(&result, ",").to_string();

    // Remove comma immediately followed by period: ",." -> "."
    let re_comma_period = Regex::new(r",\s*\.").expect("regex must compile");
    result = re_comma_period.replace_all(&result, ".").to_string();

    // Remove period immediately followed by comma: ".," -> "."
    let re_period_comma = Regex::new(r"\.\s*,").expect("regex must compile");
    result = re_period_comma.replace_all(&result, ".").to_string();

    // Remove leading comma after period-space: ". , text" -> ". text"
    let re_period_space_comma = Regex::new(r"\.\s+,\s*").expect("regex must compile");
    result = re_period_space_comma.replace_all(&result, ". ").to_string();

    result
}

/// Collapse multiple consecutive spaces into one.
fn collapse_spaces(text: &str) -> String {
    let re = Regex::new(r" {2,}").expect("regex must compile");
    re.replace_all(text, " ").to_string()
}

/// Trim leading punctuation and whitespace from the result.
/// After filler removal, text might start with ", " or similar artifacts.
fn trim_leading_punctuation(text: &str) -> String {
    let re = Regex::new(r"^[\s,;.!?]+").expect("regex must compile");
    re.replace(text, "").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_fillers_removed() {
        let input = "So basically, you know, I think we should go";
        let result = remove_fillers(input, "en");
        assert!(!result.contains("basically"));
        assert!(!result.contains("you know"));
        assert!(result.contains("I think"));
        assert!(result.contains("we should go"));
    }

    #[test]
    fn italian_fillers_removed() {
        let input = "Cioè, praticamente, insomma, andiamo al mare";
        let result = remove_fillers(input, "it");
        assert!(!result.to_lowercase().contains("cioè"));
        assert!(!result.to_lowercase().contains("praticamente"));
        assert!(result.contains("andiamo al mare"));
    }

    #[test]
    fn empty_text_returns_empty() {
        assert_eq!(remove_fillers("", "en"), "");
    }

    #[test]
    fn unknown_language_defaults_to_english() {
        let input = "So basically we should go";
        let result = remove_fillers(input, "xx");
        assert!(!result.contains("basically"));
    }

    #[test]
    fn preserves_non_filler_content() {
        let input = "The weather is nice today";
        let result = remove_fillers(input, "en");
        assert_eq!(result.trim(), "The weather is nice today");
    }

    #[test]
    fn cleans_orphaned_punctuation() {
        let input = "So, basically, we go";
        let result = remove_fillers(input, "en");
        assert!(!result.contains(", ,"));
    }

    #[test]
    fn case_insensitive() {
        let input = "BASICALLY we should go";
        let result = remove_fillers(input, "en");
        assert!(!result.to_lowercase().contains("basically"));
    }

    // Additional edge-case tests

    #[test]
    fn short_filler_not_removed_from_middle_of_word() {
        // "so" should not be removed from "someone" or "also"
        let input = "Someone also thinks it is good";
        let result = remove_fillers(input, "en");
        assert!(result.contains("Someone"));
        assert!(result.contains("also"));
    }

    #[test]
    fn spanish_fillers_removed() {
        let input = "O sea, digamos, vamos a la playa";
        let result = remove_fillers(input, "es");
        assert!(!result.to_lowercase().contains("o sea"));
        assert!(!result.to_lowercase().contains("digamos"));
        assert!(result.contains("vamos a la playa"));
    }

    #[test]
    fn french_fillers_removed() {
        let input = "En fait, du coup, on va au cinéma";
        let result = remove_fillers(input, "fr");
        assert!(!result.to_lowercase().contains("en fait"));
        assert!(!result.to_lowercase().contains("du coup"));
        assert!(result.contains("on va au cinéma"));
    }

    #[test]
    fn german_fillers_removed() {
        let input = "Eigentlich, sozusagen, gehen wir ins Kino";
        let result = remove_fillers(input, "de");
        assert!(!result.to_lowercase().contains("eigentlich"));
        assert!(!result.to_lowercase().contains("sozusagen"));
        assert!(result.contains("gehen wir ins Kino"));
    }

    #[test]
    fn portuguese_fillers_removed() {
        let input = "Quer dizer, digamos, vamos à praia";
        let result = remove_fillers(input, "pt");
        assert!(!result.to_lowercase().contains("quer dizer"));
        assert!(!result.to_lowercase().contains("digamos"));
        assert!(result.contains("vamos à praia"));
    }

    #[test]
    fn no_double_spaces_in_result() {
        let input = "Um, basically, you know, I think we should go";
        let result = remove_fillers(input, "en");
        assert!(
            !result.contains("  "),
            "result should not have double spaces: '{}'",
            result
        );
    }

    #[test]
    fn all_fillers_returns_empty_or_clean() {
        let input = "basically, you know, um";
        let result = remove_fillers(input, "en");
        // Should be empty or just whitespace after removing everything
        assert!(result.trim().is_empty() || !result.to_lowercase().contains("basically"));
    }

    #[test]
    fn multiword_filler_removed_before_subparts() {
        // "you know" should be removed as a unit, not partially
        let input = "you know that is important";
        let result = remove_fillers(input, "en");
        assert!(!result.contains("you know"));
        assert!(result.contains("that is important"));
    }
}

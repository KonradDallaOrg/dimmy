//! App-context rules — auto-switch the LLM rewrite style based on which
//! application had focus when the user pressed the hotkey.
//!
//! Design: at hotkey-down, the platform layer captures the foreground app's
//! identifier (Windows process name, macOS bundle id, Linux X11 WM_CLASS) and
//! passes it to the recording session. When LLM enhancement runs, the matcher
//! resolves the captured app id against the rule list — first match wins —
//! and produces a per-transcription override of `llm_style` and
//! `llm_translate_to` without mutating the user's saved defaults.
//!
//! Privacy: app identifiers are user data, stored locally in `config.json`.
//! They MUST NOT leave the machine via telemetry — see `CLAUDE.md` privacy
//! hard rules. Only emit rule-matched as a boolean signal.

use serde::{Deserialize, Serialize};

/// What kind of identifier `match_pattern` matches against. Picked at rule
/// creation time based on the user's OS — the matcher dispatches on this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchType {
    /// Windows process name, e.g. "slack.exe", "code.exe", "outlook.exe".
    /// Case-insensitive comparison; `.exe` suffix is normalised away.
    ProcessName,
    /// macOS bundle identifier, e.g. "com.tinyspeck.slackmacgap",
    /// "com.microsoft.VSCode". Exact match, case-sensitive (Apple convention).
    BundleId,
    /// X11 WM_CLASS, e.g. "Slack", "Code", "Outlook". Case-insensitive.
    /// Wayland is unsupported (compositor security model) — rules with this
    /// match_type silently no-op on Wayland sessions.
    WmClass,
}

/// One rule entry. The first rule whose pattern matches the captured app id
/// wins; remaining rules are ignored. Rules are stored in user-visible order
/// in `config.json` so the user can reorder them in the Settings UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppRule {
    /// Pattern to match against. Exact-match string for now; future versions
    /// may add glob support (e.g. `chrome*.exe`) once we see real-world need.
    pub match_pattern: String,
    pub match_type: MatchType,
    /// LLM style to apply when this rule matches. Empty string means "leave
    /// user's default" — useful for `match=code.exe → translate_to=""` rules
    /// that only override translation, not style.
    pub llm_style: String,
    /// Translation target ISO code, or "" for "no translation". Empty string
    /// here is meaningful — distinct from "leave user default" — because the
    /// user might want a rule that *forces* translation off in code editors.
    /// To express "leave default", omit the field entirely (use the
    /// `Option`-aware loader).
    #[serde(default)]
    pub llm_translate_to: Option<String>,
    /// Human-friendly label shown in the Settings UI list. Optional — falls
    /// back to `match_pattern` for display when empty.
    #[serde(default)]
    pub label: String,
    /// Disabled rules stay in the list but are skipped during matching. Lets
    /// the user toggle a rule off without losing its config.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

/// Foreground-app snapshot captured by the platform layer at hotkey-down.
/// Populated by the C# / Swift / GTK frontend before the recording starts;
/// passed through FFI to the Rust core so the LLM-enhance step can resolve
/// rules without having to query the OS itself.
#[derive(Debug, Clone, Default)]
pub struct AppContext {
    /// Windows process name (e.g. "slack.exe"), populated on Windows. Empty
    /// on other platforms.
    pub process_name: String,
    /// macOS bundle id (e.g. "com.tinyspeck.slackmacgap"), populated on
    /// macOS. Empty on other platforms.
    pub bundle_id: String,
    /// X11 WM_CLASS, populated on Linux X11 sessions. Empty on Wayland and
    /// other platforms.
    pub wm_class: String,
}

impl AppContext {
    /// True when no platform identifier is set — happens on Wayland (no API
    /// to query foreground app) or when the platform layer simply didn't
    /// capture a snapshot. The matcher treats this as "no rule matches",
    /// preserving the user's defaults.
    pub fn is_empty(&self) -> bool {
        self.process_name.is_empty() && self.bundle_id.is_empty() && self.wm_class.is_empty()
    }
}

/// Override produced by `resolve` when a rule matches. `None` means the
/// caller should keep the user's saved defaults.
#[derive(Debug, Clone, Default)]
pub struct RuleOverride {
    pub llm_style: Option<String>,
    pub llm_translate_to: Option<String>,
    /// Index of the matched rule in the original list — useful for telemetry
    /// (counts of rule-N matches) without sending the rule contents
    /// themselves.
    pub matched_rule_index: Option<usize>,
}

impl RuleOverride {
    pub fn is_empty(&self) -> bool {
        self.llm_style.is_none() && self.llm_translate_to.is_none()
    }
}

/// Find the first enabled rule that matches the captured app context. First
/// match wins. Returns an empty `RuleOverride` when nothing matches — the
/// caller then uses the user's defaults unchanged.
pub fn resolve(rules: &[AppRule], ctx: &AppContext) -> RuleOverride {
    assert!(
        rules.len() < 1024,
        "resolve: rule list exceeds 1024 entries — probable config corruption"
    );

    if ctx.is_empty() {
        return RuleOverride::default();
    }

    for (idx, rule) in rules.iter().enumerate() {
        if !rule.enabled {
            continue;
        }
        if !matches(rule, ctx) {
            continue;
        }

        let mut over = RuleOverride {
            matched_rule_index: Some(idx),
            ..Default::default()
        };
        if !rule.llm_style.is_empty() {
            over.llm_style = Some(rule.llm_style.clone());
        }
        if let Some(t) = &rule.llm_translate_to {
            over.llm_translate_to = Some(t.clone());
        }
        return over;
    }
    RuleOverride::default()
}

fn matches(rule: &AppRule, ctx: &AppContext) -> bool {
    match rule.match_type {
        MatchType::ProcessName => normalise_process_name(&ctx.process_name)
            .eq_ignore_ascii_case(&normalise_process_name(&rule.match_pattern)),
        MatchType::BundleId => ctx.bundle_id == rule.match_pattern,
        MatchType::WmClass => ctx.wm_class.eq_ignore_ascii_case(&rule.match_pattern),
    }
}

/// Strip a trailing `.exe` (case-insensitive) so users can write either
/// `slack` or `slack.exe` and get the same match.
fn normalise_process_name(name: &str) -> &str {
    if name.len() >= 4 && name[name.len() - 4..].eq_ignore_ascii_case(".exe") {
        &name[..name.len() - 4]
    } else {
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(pattern: &str, mt: MatchType, style: &str) -> AppRule {
        AppRule {
            match_pattern: pattern.to_string(),
            match_type: mt,
            llm_style: style.to_string(),
            llm_translate_to: None,
            label: String::new(),
            enabled: true,
        }
    }

    fn ctx_process(name: &str) -> AppContext {
        AppContext {
            process_name: name.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn empty_context_yields_no_override() {
        let rules = [rule("slack.exe", MatchType::ProcessName, "casual")];
        let over = resolve(&rules, &AppContext::default());
        assert!(over.is_empty());
        assert!(over.matched_rule_index.is_none());
    }

    #[test]
    fn process_name_matches_case_insensitive_with_or_without_exe() {
        let rules = [rule("slack", MatchType::ProcessName, "casual")];
        assert_eq!(
            resolve(&rules, &ctx_process("Slack.exe"))
                .llm_style
                .as_deref(),
            Some("casual")
        );
        assert_eq!(
            resolve(&rules, &ctx_process("slack.exe"))
                .llm_style
                .as_deref(),
            Some("casual")
        );
        assert_eq!(
            resolve(&rules, &ctx_process("SLACK")).llm_style.as_deref(),
            Some("casual")
        );
    }

    #[test]
    fn first_match_wins() {
        let rules = [
            rule("slack.exe", MatchType::ProcessName, "casual"),
            rule("slack.exe", MatchType::ProcessName, "professional"),
        ];
        let over = resolve(&rules, &ctx_process("slack.exe"));
        assert_eq!(over.llm_style.as_deref(), Some("casual"));
        assert_eq!(over.matched_rule_index, Some(0));
    }

    #[test]
    fn disabled_rule_is_skipped() {
        let mut r = rule("slack.exe", MatchType::ProcessName, "casual");
        r.enabled = false;
        let over = resolve(&[r], &ctx_process("slack.exe"));
        assert!(over.is_empty());
    }

    #[test]
    fn empty_style_means_keep_default_but_translate_can_still_override() {
        let mut r = rule("code.exe", MatchType::ProcessName, "");
        r.llm_translate_to = Some("".to_string());
        let over = resolve(&[r], &ctx_process("code.exe"));
        assert!(over.llm_style.is_none());
        assert_eq!(over.llm_translate_to.as_deref(), Some(""));
        assert!(over.matched_rule_index.is_some());
    }

    #[test]
    fn bundle_id_match_is_case_sensitive() {
        let r = rule("com.tinyspeck.slackmacgap", MatchType::BundleId, "casual");
        let mut ctx = AppContext::default();
        ctx.bundle_id = "com.tinyspeck.slackmacgap".to_string();
        assert!(!resolve(&[r.clone()], &ctx).is_empty());

        ctx.bundle_id = "COM.TINYSPECK.SLACKMACGAP".to_string();
        assert!(resolve(&[r], &ctx).is_empty());
    }
}

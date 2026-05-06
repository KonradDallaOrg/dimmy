import Foundation
import SwiftUI

// MARK: - AppRule (mirror of Rust app_rules::AppRule)
//
// Each rule is evaluated top-down at hotkey-down time. The first rule
// whose `matchPattern` matches the captured app context wins. On macOS
// the captured field is `bundle_id`; rules created from the v1 defaults
// use match_type = "bundle_id" so they match bundle IDs exactly.
//
// `enabled = false` keeps a rule in the list but skips it during
// matching, useful for the user to toggle a rule off without losing the
// config. `llmStyle = ""` means "leave the user's default style alone";
// `llmTranslateTo = ""` means "force no translation" (distinct from
// nil which means "leave default translation alone" — Rust mirrors
// this with an Option<String>).

struct AppRule: Identifiable, Equatable {
    /// Stable client-side ID for SwiftUI list reorder. Not persisted —
    /// the Rust side has no ID concept; rules are identified by their
    /// position in the `app_rules` array.
    var id: UUID = UUID()
    /// Pattern to match against (e.g. "com.tinyspeck.slackmacgap").
    var matchPattern: String
    /// One of: "process_name", "bundle_id", "wm_class". Mac defaults
    /// always use "bundle_id".
    var matchType: String
    /// LLM style key (one of `MacLlmStyles`). Empty means "no override".
    var llmStyle: String
    /// Translation ISO code, "" = force no translation, nil = no override.
    var llmTranslateTo: String?
    /// Human-friendly label shown in the list.
    var label: String
    var enabled: Bool

    init(matchPattern: String,
         matchType: String,
         llmStyle: String,
         llmTranslateTo: String? = nil,
         label: String,
         enabled: Bool = true) {
        self.matchPattern = matchPattern
        self.matchType = matchType
        self.llmStyle = llmStyle
        self.llmTranslateTo = llmTranslateTo
        self.label = label
        self.enabled = enabled
    }

    /// Decode from the Rust JSON shape. Tolerant of missing fields.
    init?(dict: [String: Any]) {
        guard let pattern = dict["match_pattern"] as? String else { return nil }
        let mt = (dict["match_type"] as? String) ?? "bundle_id"
        self.matchPattern = pattern
        self.matchType = mt
        self.llmStyle = (dict["llm_style"] as? String) ?? ""
        self.llmTranslateTo = dict["llm_translate_to"] as? String
        self.label = (dict["label"] as? String) ?? ""
        self.enabled = (dict["enabled"] as? Bool) ?? true
    }

    /// Encode for the Rust JSON shape. `llm_translate_to` is omitted
    /// when nil so Rust's Option<String> deserialiser sees `None`.
    func toDict() -> [String: Any] {
        var d: [String: Any] = [
            "match_pattern": matchPattern,
            "match_type": matchType,
            "llm_style": llmStyle,
            "label": label,
            "enabled": enabled,
        ]
        if let t = llmTranslateTo { d["llm_translate_to"] = t }
        return d
    }

    /// Category inferred from the bundle id, used to pick an SF Symbol
    /// icon and a tile background colour. Mirrors the WinUI Glyph mapping
    /// in `App rules` page (CategoryToGlyphConverter).
    var category: AppRuleCategory {
        AppRuleCategory.from(matchPattern: matchPattern, matchType: matchType)
    }
}

// MARK: - AppRuleIcon (real Mac icon when available, SF Symbol fallback)
//
// SwiftUI view that prefers the real installed-app icon (transparent
// background, original Apple-quality glyph) and falls back to a
// category SF Symbol on a coloured squircle when the app isn't on this
// machine. Sized to match MacSquircleIcon in adjacent rows.

struct AppRuleIcon: View {
    let rule: AppRule
    var size: CGFloat = 28

    var body: some View {
        if rule.matchType == "bundle_id",
           let nsImage = AppContextCapture.appIcon(for: rule.matchPattern) {
            // Real .app icon — already sized correctly by IconServices,
            // background is transparent. Just render at our target size.
            Image(nsImage: nsImage)
                .resizable()
                .interpolation(.high)
                .aspectRatio(contentMode: .fit)
                .frame(width: size, height: size)
        } else {
            MacSquircleIcon(
                systemName: rule.category.systemImage,
                background: rule.category.color,
                size: size
            )
        }
    }
}

// MARK: - AppRuleCategory (icon + colour mapping)

enum AppRuleCategory {
    case chat, mail, browser, code, document, terminal, generic

    var systemImage: String {
        switch self {
        case .chat:     return "bubble.left.and.bubble.right.fill"
        case .mail:     return "envelope.fill"
        case .browser:  return "globe"
        case .code:     return "chevron.left.forwardslash.chevron.right"
        case .document: return "doc.text.fill"
        case .terminal: return "terminal.fill"
        case .generic:  return "app.fill"
        }
    }

    var color: Color {
        switch self {
        case .chat:     return Color(red: 0.10, green: 0.69, blue: 0.45)  // green
        case .mail:     return Color(red: 0.04, green: 0.52, blue: 1.00)  // blue
        case .browser:  return Color(red: 0.34, green: 0.61, blue: 0.99)  // light blue
        case .code:     return Color(red: 0.56, green: 0.56, blue: 0.58)  // grey
        case .document: return Color(red: 1.00, green: 0.62, blue: 0.04)  // orange
        case .terminal: return Color(red: 0.28, green: 0.28, blue: 0.30)  // dark grey
        case .generic:  return Color(red: 0.69, green: 0.32, blue: 0.87)  // purple
        }
    }

    /// Pattern → category. Recognises both Win process_name patterns
    /// (e.g. "slack.exe") and Mac bundle_id patterns (e.g.
    /// "com.tinyspeck.slackmacgap"), so the same logic works regardless
    /// of which platform created the rule.
    static func from(matchPattern: String, matchType: String) -> AppRuleCategory {
        let p = matchPattern.lowercased()
        // Mail clients first — distinct from chat (Outlook etc).
        if p.contains("mail") || p.contains("outlook") || p.contains("thunderbird") {
            return .mail
        }
        if p.contains("slack") || p.contains("discord") || p.contains("whatsapp")
            || p.contains("telegram") || p.contains("teams") || p.contains("messenger")
            || p.contains("messages") {
            return .chat
        }
        if p.contains("safari") || p.contains("chrome") || p.contains("firefox")
            || p.contains("edge") || p.contains("brave") || p.contains("arc")
            || p.contains("thebrowser") || p.contains("vivaldi") {
            return .browser
        }
        if p.contains("vscode") || p.contains("code.exe") || p.contains("cursor")
            || p.contains("xcode") || p.contains("idea") || p.contains("sublime")
            || p.contains("pycharm") || p.contains("jetbrains") {
            return .code
        }
        if p.contains("terminal") || p.contains("iterm") || p.contains("warp") {
            return .terminal
        }
        if p.contains("notion") || p.contains("obsidian") || p.contains("word")
            || p.contains("notes") || p.contains("pages") || p.contains("bear") {
            return .document
        }
        return .generic
    }
}

// MARK: - AppRulesDefaults (v1 baseline shipped with macOS)
//
// Mirror of `AppRulesDefaults.V1Windows` but keyed on Mac bundle IDs.
// Users surface this list via the "Load defaults" button on the App
// rules page. Order matters for clarity (chat first, work mail second,
// browsers third, code/dev last) but not for matching — bundle IDs are
// unique so the first-match-wins ordering is irrelevant here.

enum AppRulesDefaults {
    static let macV1: [AppRule] = [
        // — Chat / messaging — casual / playful register —
        AppRule(matchPattern: "com.tinyspeck.slackmacgap", matchType: "bundle_id",
                llmStyle: "imbruttito", label: "Slack"),
        AppRule(matchPattern: "com.hnc.Discord", matchType: "bundle_id",
                llmStyle: "genz", label: "Discord"),
        AppRule(matchPattern: "net.whatsapp.WhatsApp", matchType: "bundle_id",
                llmStyle: "imbruttito", label: "WhatsApp"),
        AppRule(matchPattern: "ru.keepcoder.Telegram", matchType: "bundle_id",
                llmStyle: "imbruttito", label: "Telegram"),

        // — Work chat / mail — professional —
        AppRule(matchPattern: "com.microsoft.teams2", matchType: "bundle_id",
                llmStyle: "professional", label: "Microsoft Teams"),
        AppRule(matchPattern: "com.microsoft.Outlook", matchType: "bundle_id",
                llmStyle: "professional", label: "Outlook"),
        AppRule(matchPattern: "com.apple.mail", matchType: "bundle_id",
                llmStyle: "professional", label: "Apple Mail"),

        // — Browsers — light grammar fix —
        AppRule(matchPattern: "com.apple.Safari", matchType: "bundle_id",
                llmStyle: "correct", label: "Safari"),
        AppRule(matchPattern: "com.google.Chrome", matchType: "bundle_id",
                llmStyle: "correct", label: "Chrome"),
        AppRule(matchPattern: "org.mozilla.firefox", matchType: "bundle_id",
                llmStyle: "correct", label: "Firefox"),
        AppRule(matchPattern: "com.brave.Browser", matchType: "bundle_id",
                llmStyle: "correct", label: "Brave"),
        AppRule(matchPattern: "company.thebrowser.Browser", matchType: "bundle_id",
                llmStyle: "correct", label: "Arc"),

        // — Code editors / IDEs — no LLM —
        AppRule(matchPattern: "com.microsoft.VSCode", matchType: "bundle_id",
                llmStyle: "off", label: "VS Code"),
        AppRule(matchPattern: "com.todesktop.230313mzl4w4u92", matchType: "bundle_id",
                llmStyle: "off", label: "Cursor"),
        AppRule(matchPattern: "com.apple.dt.Xcode", matchType: "bundle_id",
                llmStyle: "off", label: "Xcode"),
        AppRule(matchPattern: "com.apple.Notes", matchType: "bundle_id",
                llmStyle: "off", label: "Notes"),

        // — Document writing — comprehensible / polish —
        AppRule(matchPattern: "com.microsoft.Word", matchType: "bundle_id",
                llmStyle: "comprehensible", label: "Microsoft Word"),
        AppRule(matchPattern: "notion.id", matchType: "bundle_id",
                llmStyle: "comprehensible", label: "Notion"),
        AppRule(matchPattern: "md.obsidian", matchType: "bundle_id",
                llmStyle: "comprehensible", label: "Obsidian"),
    ]
}

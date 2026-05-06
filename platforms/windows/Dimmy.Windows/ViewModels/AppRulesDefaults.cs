using System.Collections.Generic;

namespace Dimmy.Windows.ViewModels;

/// Hardcoded baseline app rules versioned by snapshot. v1 is the
/// initial set surfaced via "Load defaults". The list is intentionally
/// short (1 row per common app category) — users curate further via
/// the Settings UI. Mac uses bundle_id; Windows here uses process_name
/// because that's what `AppContextCapture.GetForegroundProcessName`
/// surfaces. Future v2 expands or refines.
public static class AppRulesDefaults
{
    public static readonly IReadOnlyList<AppRuleViewModel> V1Windows = new[]
    {
        // — Chat / messaging — casual register —
        new AppRuleViewModel("slack.exe",       "process_name", "imbruttito",     "", "Slack",     true),
        new AppRuleViewModel("discord.exe",     "process_name", "genz",           "", "Discord",   true),
        new AppRuleViewModel("whatsapp.exe",    "process_name", "imbruttito",     "", "WhatsApp",  true),
        new AppRuleViewModel("telegram.exe",    "process_name", "imbruttito",     "", "Telegram",  true),

        // — Work chat / mail — professional —
        new AppRuleViewModel("teams.exe",       "process_name", "professional",   "", "Teams",     true),
        new AppRuleViewModel("ms-teams.exe",    "process_name", "professional",   "", "Teams (new)", true),
        new AppRuleViewModel("outlook.exe",     "process_name", "professional",   "", "Outlook",   true),
        new AppRuleViewModel("thunderbird.exe", "process_name", "professional",   "", "Thunderbird", true),

        // — Browsers — light grammar fix —
        new AppRuleViewModel("chrome.exe",      "process_name", "correct",        "", "Chrome",    true),
        new AppRuleViewModel("msedge.exe",      "process_name", "correct",        "", "Edge",      true),
        new AppRuleViewModel("firefox.exe",     "process_name", "correct",        "", "Firefox",   true),
        new AppRuleViewModel("brave.exe",       "process_name", "correct",        "", "Brave",     true),

        // — Code editors / terminals — no LLM —
        new AppRuleViewModel("code.exe",        "process_name", "off",            "", "VS Code",   true),
        new AppRuleViewModel("notepad.exe",     "process_name", "off",            "", "Notepad",   true),
        new AppRuleViewModel("notepad++.exe",   "process_name", "off",            "", "Notepad++", true),
        new AppRuleViewModel("windowsterminal.exe","process_name", "off",         "", "Windows Terminal", true),
        new AppRuleViewModel("cursor.exe",      "process_name", "off",            "", "Cursor",    true),

        // — Document writing — comprehensible / polish —
        new AppRuleViewModel("winword.exe",     "process_name", "comprehensible", "", "Word",      true),
        new AppRuleViewModel("notion.exe",      "process_name", "comprehensible", "", "Notion",    true),
        new AppRuleViewModel("obsidian.exe",    "process_name", "comprehensible", "", "Obsidian",  true),
    };
}

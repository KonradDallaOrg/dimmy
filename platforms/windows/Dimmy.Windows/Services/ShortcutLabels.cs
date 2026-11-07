using System;
using System.Collections.Generic;
using System.Linq;

namespace Dimmy.Windows.Services;

/// <summary>The keys of a stored shortcut, as labels to draw. Pure, so the
/// headless test project can cover it.</summary>
public static class ShortcutLabels
{
    private static readonly string[] ModifierOrder = { "Win", "Ctrl", "Alt", "Shift" };

    /// <summary>Labels for a stored combo, modifiers first in the Windows order.
    /// Also reads what older versions saved: WinUI key names (<c>LeftWindows</c>,
    /// <c>Number1</c>, <c>Escape</c>) and the macOS spellings.</summary>
    public static List<string> FromCombo(string? combo)
    {
        var mods = new HashSet<string>();
        string? key = null;
        foreach (var raw in (combo ?? "").Split('+', StringSplitOptions.TrimEntries | StringSplitOptions.RemoveEmptyEntries))
        {
            var p = raw.ToLowerInvariant();
            switch (p)
            {
                case "win" or "cmd" or "super" or "meta" or "leftwindows" or "rightwindows": mods.Add("Win"); break;
                case "ctrl" or "control" or "leftcontrol" or "rightcontrol": mods.Add("Ctrl"); break;
                case "alt" or "option" or "opt" or "menu" or "leftmenu" or "rightmenu": mods.Add("Alt"); break;
                case "shift" or "leftshift" or "rightshift": mods.Add("Shift"); break;
                default:
                    key = p.StartsWith("number") && p.Length == 7 ? p[6..]
                        : p == "escape" ? "Esc"
                        : char.ToUpperInvariant(p[0]) + p[1..];
                    break;
            }
        }
        var labels = ModifierOrder.Where(mods.Contains).ToList();
        if (key != null) labels.Add(key);
        return labels;
    }
}

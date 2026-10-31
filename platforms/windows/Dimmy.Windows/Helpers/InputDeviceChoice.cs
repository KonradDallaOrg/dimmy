using System;
using System.Collections.Generic;
using System.Linq;

namespace Dimmy.Windows.Helpers;

/// <summary>What the input device picker offers and what each entry saves.
/// The core stores no name for "System default", and an empty value in a save
/// could also come from a view model that has not loaded yet, so the entry has
/// its own text and only that text clears the choice.</summary>
public static class InputDeviceChoice
{
    public const string SystemDefault = "System default";

    public static string ForDisplay(string? saved) =>
        string.IsNullOrEmpty(saved) ? SystemDefault : saved;

    /// <summary>A saved device that is not connected stays in the list: it comes
    /// back when it is plugged in again, and until then the picker tells the
    /// truth about what is saved.</summary>
    public static List<string> Entries(IEnumerable<string> connected, string? selected)
    {
        ArgumentNullException.ThrowIfNull(connected);
        var entries = new List<string> { SystemDefault };
        entries.AddRange(connected);
        if (!string.IsNullOrEmpty(selected) && !entries.Contains(selected)) entries.Add(selected);
        return entries;
    }

    /// <summary>The value for the core's config: empty clears the device, null
    /// leaves the saved choice alone.</summary>
    public static string? ConfigValue(string? selected) =>
        selected == SystemDefault ? "" : string.IsNullOrEmpty(selected) ? null : selected;

    /// <summary>The name to show as in use: capture falls back to the system
    /// default when the saved device is not connected.</summary>
    public static string InUse(string? selected, IEnumerable<string> connected) =>
        !string.IsNullOrEmpty(selected) && connected.Contains(selected) ? selected : SystemDefault;
}

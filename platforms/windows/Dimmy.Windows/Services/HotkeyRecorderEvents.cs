using System;
using System.Collections.Generic;
using System.Text.Json;

namespace Dimmy.Windows.Services;

/// <summary>One <c>hotkey_recorder</c> event from the core: the keys held at
/// the last key-down and what the core thinks of them.</summary>
public sealed record RecorderVerdict(
    string Target,
    IReadOnlyList<string> Keys,
    string Combo,
    bool CanSave,
    string Error,
    IReadOnlyList<string> Warnings)
{
    public static RecorderVerdict FromJson(JsonElement p)
    {
        static List<string> Strings(JsonElement arr)
        {
            var list = new List<string>();
            foreach (var e in arr.EnumerateArray())
                list.Add(e.GetString() ?? "");
            return list;
        }

        return new RecorderVerdict(
            p.GetProperty("target").GetString() ?? "",
            Strings(p.GetProperty("keys")),
            p.GetProperty("combo").GetString() ?? "",
            p.GetProperty("can_save").GetBoolean(),
            p.GetProperty("error").GetString() ?? "",
            Strings(p.GetProperty("warnings")));
    }
}

/// <summary>Hands <c>hotkey_recorder</c> events from
/// <see cref="ViewModels.AppViewModel.HandleEvent"/> to the shortcut dialog
/// that is open. Raised on the UI thread.</summary>
public static class HotkeyRecorderEvents
{
    public static event Action<RecorderVerdict>? Received;

    public static void Publish(JsonElement payload) => Received?.Invoke(RecorderVerdict.FromJson(payload));
}

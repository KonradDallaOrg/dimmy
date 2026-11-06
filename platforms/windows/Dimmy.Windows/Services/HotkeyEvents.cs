using System;

namespace Dimmy.Windows.Services;

/// <summary>Hands <c>hotkey</c> events (one per press or release of a
/// shortcut) from <see cref="ViewModels.AppViewModel.HandleEvent"/> to
/// <see cref="HotkeyService"/>. Raised on the UI thread.</summary>
public static class HotkeyEvents
{
    /// <summary>(binding, pressed): binding is <c>dictation</c> |
    /// <c>command</c> | <c>meeting</c>; pressed is false for a release.</summary>
    public static event Action<string, bool>? Received;

    public static void Publish(string binding, string edge) => Received?.Invoke(binding, edge == "pressed");
}

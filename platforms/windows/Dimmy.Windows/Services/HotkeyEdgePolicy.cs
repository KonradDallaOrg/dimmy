namespace Dimmy.Windows.Services;

/// <summary>Which shortcut edges reach the app. Pure, so the headless test
/// project covers it.</summary>
public static class HotkeyEdgePolicy
{
    /// <summary>Whether a release of <paramref name="binding"/> should reach
    /// the app. Only in push-to-talk (<c>shortcut_mode == "hold"</c>), and
    /// never for the meeting shortcut: a meeting can't be push-to-talk.
    /// The caller passes the mode as it is NOW, never a copy.</summary>
    public static bool DeliverRelease(string binding, string shortcutMode) =>
        binding is "dictation" or "command" && shortcutMode == "hold";
}

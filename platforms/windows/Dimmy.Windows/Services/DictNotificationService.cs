using System;
using Microsoft.UI.Dispatching;

namespace Dimmy.Windows.Services;

/// <summary>
/// Brief in-app toast feedback when a word is added to / already in the
/// user dictionary. Uses a bespoke <see cref="Views.DictToastWindow"/>
/// rather than <c>Microsoft.Windows.AppNotifications</c>: unpackaged
/// WinAppSDK apps can't surface AppNotifications reliably without a
/// pre-registered ToastActivator COM CLSID (silent-drop verified
/// 2026-05-11). The bespoke window has zero registration requirements
/// and matches the pill / caption window pattern we already use.
/// </summary>
public static class DictNotificationService
{
    /// <summary>Show "+ Added 'word' to dictionary" — auto-dismisses
    /// after ~3 s. Must be called on the UI dispatcher thread; falls
    /// back to a marshal call when invoked from a worker.</summary>
    public static void ShowAdded(string word)
    {
        Show("Added to dictionary",
             $"“{word}” will boost recognition on future transcriptions.");
    }

    /// <summary>Soft feedback when the user re-adds a word that's
    /// already on the list (dimmy_user_dict_add returns rc=1). Keeps
    /// the hotkey from feeling broken on accidental double-press.</summary>
    public static void ShowAlreadyPresent(string word)
    {
        Show("Already in dictionary",
             $"“{word}” is already on the list.");
    }

    private static void Show(string title, string body)
    {
        try
        {
            void Open()
            {
                try
                {
                    var w = new Views.DictToastWindow(title, body);
                    w.Activate();
                }
                catch (Exception ex)
                {
                    App.Log($"DictToast open failed: {ex.Message}", "Dict");
                }
            }
            // If we're already on the UI thread, open now; otherwise
            // marshal. The DictHotkey handler already runs on the UI
            // dispatcher (App._dispatcherQueue.TryEnqueue), so the
            // direct path is the common case.
            var dq = DispatcherQueue.GetForCurrentThread();
            if (dq is not null) Open();
            else App.Instance?.RunOnUI(Open);
        }
        catch (Exception ex)
        {
            App.Log($"DictNotification Show exc: {ex.Message}", "Dict");
        }
    }
}

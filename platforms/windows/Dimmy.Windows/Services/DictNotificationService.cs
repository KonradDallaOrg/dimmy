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

    /// <summary>Post-update confirmation — fires once on the first
    /// launch after Velopack swapped the EXE. Without this, users
    /// who clicked "Install Now" saw the app close (Velopack kills
    /// the old process) and reasonably wondered whether the new
    /// version had actually come up. The marker file the toast
    /// reads is written by UpdateService.ApplyAndRestart and
    /// consumed exactly once at startup.</summary>
    public static void ShowUpdateInstalled(string version)
    {
        Show($"Updated to v{version}",
             "Dimmy was upgraded in the background and is ready to use.");
    }

    /// <summary>The chosen command-mode hotkey overlaps the dictation (or
    /// dictionary) shortcut, so pressing it would trigger both. Tell the user
    /// to pick a different one instead of binding a combo that double-fires.</summary>
    public static void ShowHotkeyConflict(string combo)
    {
        Show("Shortcut conflict",
             $"“{combo}” overlaps another Dimmy shortcut. Pick a different command mode shortcut.");
    }

    /// <summary>Soft nudge at the 5 min mark of a single dictation: a
    /// marathon dictation buffers unbounded audio (no draining worker),
    /// so steer the user toward Meeting mode before it grows large.</summary>
    public static void ShowLongDictationWarning()
    {
        Show("Long dictation",
             "You have been dictating for a while. For long recordings, Meeting mode is the better tool.");
    }

    /// <summary>A meeting's audio file could not be finalised (classically
    /// disk-full while rewriting the header): the recording on disk is
    /// incomplete. Without this the user finds out only when the recap
    /// mysteriously fails — audit 2026-07-02, "no silent failures".</summary>
    public static void ShowMeetingAudioIncomplete(string detail)
    {
        Show("Meeting audio incomplete",
             $"The recording could not be finalised ({detail}). Check free disk space; the transcript may still be usable.");
    }

    /// <summary>Command mode ran with no captured selection: the spoken
    /// words are executed as a generate-at-cursor instruction. Saying so
    /// kills the "it just transcribed me" confusion when a selection
    /// silently failed to capture (colleague report, 2026-07-03).</summary>
    public static void ShowCommandNoSelection()
    {
        Show("Command Mode: no text selected",
             "Executing your instruction and inserting the result at the cursor.");
    }

    /// <summary>Fired when a single dictation hits the 10 min hard cap and
    /// Dimmy auto-stops it (the audio so far is transcribed + pasted). Tells
    /// the user why it stopped and points at Meeting mode.</summary>
    public static void ShowLongDictationCapped()
    {
        Show("Dictation stopped at 10 min",
             "Dimmy stopped and transcribed what you said. For long recordings, use Meeting mode.");
    }

    /// <summary>STT failed on a dictation or command capture. The pill
    /// shows Error too, but a toast is the surface the user actually
    /// sees; name the provider, the reason, and the next step so a
    /// failing key does not read as "Dimmy is broken".</summary>
    public static void ShowTranscriptionFailed(string provider, string detail, string category)
    {
        var who = string.IsNullOrEmpty(provider) || provider == "other"
            ? "Cloud STT" : provider;
        Show($"Transcription failed ({who})", $"{detail}. {DictFailureHints.For(category)}");
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

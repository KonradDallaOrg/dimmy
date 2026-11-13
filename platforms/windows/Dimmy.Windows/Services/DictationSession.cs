using System;
using Dimmy.Windows.Interop;

namespace Dimmy.Windows.Services;

public enum SessionKind { Dictation = 0, Command = 1, Execute = 2 }

public enum SessionAction { None = 0, Start = 1, Finish = 2, Discard = 3, Busy = 4 }

/// <summary>The core's dictation session (core/src/dictation_session.rs): one
/// state machine for every way a dictation starts, stops or is thrown away.
/// The host feeds it inputs and does what it answers; it keeps no session
/// flags of its own.</summary>
public static class DictationSession
{
    public static SessionAction Press(SessionKind kind, bool hold) => Send(1, kind, hold ? 1 : 0);
    public static SessionAction Release(SessionKind kind) => Send(2, kind, 0);
    public static SessionAction Stop() => Send(3, SessionKind.Dictation, 0);
    public static void StartFailed() => Send(4, SessionKind.Dictation, 0);
    public static void Delivered() => Send(5, SessionKind.Dictation, 0);
    public static void Cancel() => Send(6, SessionKind.Dictation, 0);

    public static bool IsIdle => DimmyNative.dimmy_dictation_phase() == 0;
    public static bool IsRecording => DimmyNative.dimmy_dictation_phase() == 1;

    private static SessionAction Send(int input, SessionKind kind, int mode)
    {
        int rc = DimmyNative.dimmy_dictation_input(input, (int)kind, mode);
        if (rc < 0) throw new InvalidOperationException($"dictation session refused input {input}/{kind}/{mode}");
        return (SessionAction)rc;
    }
}

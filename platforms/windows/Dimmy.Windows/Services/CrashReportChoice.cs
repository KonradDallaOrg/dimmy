namespace Dimmy.Windows.Services;

/// <summary>
/// The answer to "Dimmy closed unexpectedly last time". "Always send" turns
/// crash reports on for the future; "Send" / "Don't send" decide only this
/// one report. Closing the window without a button is "Don't send".
/// </summary>
public readonly record struct CrashReportChoice(bool Send, bool AlwaysSend)
{
    /// The argument for <c>dimmy_crash_report_resolve</c>.
    public int ResolveArgument => Send ? 1 : 0;

    /// Shown only when the stats-consent question is not on screen too: one
    /// dialog at a time. A report left unasked waits for the next launch.
    public static bool ShouldAsk(bool statsConsentPending) => !statsConsentPending;
}

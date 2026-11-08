using System;
using System.Diagnostics;
using System.Text;

namespace Dimmy.Windows.Helpers;

/// <summary>
/// The pure half of <see cref="CrashReporter"/>: what of an exception is
/// sent. Kept free of WinUI and P/Invoke so the test project can link it.
/// </summary>
public static class CrashFrames
{
    /// <summary>An AggregateException from an unobserved task says nothing;
    /// its first inner exception is the failure.</summary>
    public static Exception Unwrap(Exception ex)
    {
        var e = ex;
        while (e is AggregateException agg && agg.InnerExceptions.Count > 0)
            e = agg.InnerExceptions[0];
        return e;
    }

    /// <summary>The type name the core gets. Never the message.</summary>
    public static string TypeName(Exception ex) => ex.GetType().FullName ?? ex.GetType().Name;

    /// <summary><c>function \t file \t line</c> per frame, innermost first.
    /// The core keeps only the file name.</summary>
    public static string Format(Exception ex)
    {
        var sb = new StringBuilder();
        var trace = new StackTrace(ex, fNeedFileInfo: true);
        foreach (var frame in trace.GetFrames())
        {
            var method = frame.GetMethod();
            if (method is null) continue;
            var type = method.DeclaringType?.FullName ?? "";
            sb.Append(type.Length > 0 ? type + "." + method.Name : method.Name);
            sb.Append('\t').Append(frame.GetFileName() ?? "");
            sb.Append('\t').Append(frame.GetFileLineNumber());
            sb.Append('\n');
        }
        return sb.ToString();
    }
}

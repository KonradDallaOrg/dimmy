using System.Collections.Generic;

namespace Dimmy.Windows.Helpers;

/// <summary>
/// The text Dimmy sends back into Telegram's Saved Messages when it has
/// finished with an audio someone shared from their phone.
///
/// <para>Pure on purpose: the person who sent the voice note is holding a
/// phone, far from this PC, and a wrong or empty reply there is the only
/// thing they see. Reading the file is the caller's job.</para>
/// </summary>
public static class TelegramReplyText
{
    /// <summary>
    /// Title plus the recap's own one-paragraph summary, taken from the
    /// rendered <c>recap.md</c>. Null when there is nothing worth quoting, so
    /// the caller can fall back to a plain confirmation rather than sending
    /// an empty message.
    /// </summary>
    public static string? FromRecapMarkdown(string? markdown)
    {
        if (string.IsNullOrWhiteSpace(markdown)) return null;

        var title = "";
        var summary = new List<string>();
        var inSummary = false;
        foreach (var raw in markdown.Split('\n'))
        {
            var line = raw.Trim();
            if (title.Length == 0 && line.StartsWith("# "))
            {
                title = line[2..].Trim();
                continue;
            }
            if (line.StartsWith("## "))
            {
                // Stop at the heading after the summary. Matching by name
                // rather than position because the recap is written in the
                // meeting's language and only the marker stays English.
                if (inSummary) break;
                var heading = line.ToLowerInvariant();
                inSummary = heading.Contains("tl;dr") || heading.Contains("tldr");
                continue;
            }
            // The AI-marking comment sits on line 2 of every recap and must
            // never be quoted back at the user.
            if (inSummary && line.Length > 0 && !line.StartsWith("<!--"))
                summary.Add(line);
        }

        var body = string.Join(" ", summary);
        if (title.Length == 0 && body.Length == 0) return null;
        if (body.Length == 0) return title;
        return title.Length == 0 ? body : $"{title}\n\n{body}";
    }
}

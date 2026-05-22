using System;
using System.Text.RegularExpressions;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Documents;
using Microsoft.UI.Xaml.Media;

namespace Dimmy.Windows.Helpers;

/// Renders meeting transcripts with light styling so a wall of text
/// becomes readable: [mic] / [system] markers turn into colored
/// badges, [hh:mm] / [00:01] timestamps render muted, regular prose
/// stays at normal weight. The card host (XAML) controls font family
/// and selectability — this helper just produces the inline tree.
public static class TranscriptRenderer
{
    // [mic] / [system] / [mic 0:01] / [system 1:23] section markers.
    // Always at the start of a line.
    private static readonly Regex SectionRe =
        new(@"^\s*\[(mic|system)(?:\s+([^\]]+))?\]\s*$",
            RegexOptions.IgnoreCase | RegexOptions.Compiled);

    // Inline timestamp markers: [0:01], [00:01], [1:23:45], [125 ms], [125ms].
    // Used inside a paragraph where text + timestamps are interleaved.
    private static readonly Regex TimestampRe =
        new(@"\[(?:\d{1,2}:\d{2}(?::\d{2})?|\d+\s*ms)\]",
            RegexOptions.Compiled);

    public static void Render(RichTextBlock target, string transcript)
    {
        target.Blocks.Clear();
        if (string.IsNullOrWhiteSpace(transcript))
        {
            var empty = new Paragraph();
            empty.Inlines.Add(new Run
            {
                Text = "(no transcript)",
                Foreground = ThemeBrush("TextFillColorSecondaryBrush"),
                FontStyle = global::Windows.UI.Text.FontStyle.Italic,
            });
            target.Blocks.Add(empty);
            return;
        }

        // We process line-by-line so [mic] / [system] markers can each
        // own their paragraph. Empty lines collapse — a single para
        // gap is enough between sections.
        var lines = transcript.Replace("\r\n", "\n").Split('\n');
        Paragraph? current = null;
        bool lastWasSection = false;

        foreach (var rawLine in lines)
        {
            var line = rawLine.TrimEnd();
            if (string.IsNullOrEmpty(line))
            {
                // Empty line ends the current paragraph.
                if (current != null)
                {
                    target.Blocks.Add(current);
                    current = null;
                }
                lastWasSection = false;
                continue;
            }

            var m = SectionRe.Match(line);
            if (m.Success)
            {
                if (current != null) { target.Blocks.Add(current); current = null; }
                target.Blocks.Add(BuildSectionBadge(m.Groups[1].Value, m.Groups[2].Value));
                lastWasSection = true;
                continue;
            }

            // Body line — accumulate into current paragraph, soft-wrapping.
            if (current == null)
            {
                current = new Paragraph
                {
                    Margin = new Thickness(0, lastWasSection ? 2 : 0, 0, 6),
                    LineHeight = 22,
                };
            }
            else
            {
                current.Inlines.Add(new LineBreak());
            }
            AppendLineWithTimestamps(current, line);
            lastWasSection = false;
        }

        if (current != null) target.Blocks.Add(current);
    }

    private static Paragraph BuildSectionBadge(string track, string? extra)
    {
        var p = new Paragraph
        {
            Margin = new Thickness(0, 8, 0, 4),
        };
        var label = track.Trim().ToLowerInvariant();
        // mic = green-mint, system = violet — same palette as the
        // dual-band live waveform colors so the user has a visual
        // continuum from recording to playback. Two foreground
        // tints per track: a LIGHT one for Dark theme (pops on
        // dark surface) and a SATURATED-DARK one for Light theme
        // (pops on white surface). The earlier code only had the
        // dark-theme tint which made the badges invisible on Light.
        bool darkTheme = IsAppDarkTheme();
        global::Windows.UI.Color bg, fg;
        if (label == "system")
        {
            bg = Microsoft.UI.ColorHelper.FromArgb(0x33, 0xA8, 0x7C, 0xFF);
            fg = darkTheme
                ? Microsoft.UI.ColorHelper.FromArgb(0xFF, 0xC9, 0xB0, 0xFF)  // pale violet on dark
                : Microsoft.UI.ColorHelper.FromArgb(0xFF, 0x55, 0x35, 0x9C); // deep violet on light
        }
        else
        {
            bg = Microsoft.UI.ColorHelper.FromArgb(0x33, 0x4A, 0xD9, 0x9D);
            fg = darkTheme
                ? Microsoft.UI.ColorHelper.FromArgb(0xFF, 0x7E, 0xE8, 0xC0)  // pale mint on dark
                : Microsoft.UI.ColorHelper.FromArgb(0xFF, 0x1B, 0x6D, 0x4F); // deep teal on light
        }

        // Render as a single Run with subtle background tint via
        // Foreground color + bold weight. WinUI Run has no Background,
        // so we lean on color contrast + uppercase letter-spacing.
        var badge = new Run
        {
            Text = string.IsNullOrWhiteSpace(extra)
                ? $"  {label.ToUpperInvariant()}  "
                : $"  {label.ToUpperInvariant()}   {extra.Trim()}  ",
            FontWeight = Microsoft.UI.Text.FontWeights.SemiBold,
            FontSize = 11,
            Foreground = new SolidColorBrush(fg),
        };
        // Trick to give the badge "fill": a thin run before/after with
        // background-tinted full-block char acts as a left/right cap.
        var leftPad = new Run
        {
            Text = "▎",
            Foreground = new SolidColorBrush(fg),
            FontSize = 12,
        };
        p.Inlines.Add(leftPad);
        p.Inlines.Add(badge);
        return p;
    }

    private static void AppendLineWithTimestamps(Paragraph p, string line)
    {
        // Walk the line, splitting at timestamp matches so each
        // timestamp becomes its own muted Run.
        int last = 0;
        var matches = TimestampRe.Matches(line);
        foreach (Match m in matches)
        {
            if (m.Index > last)
            {
                p.Inlines.Add(new Run { Text = line.Substring(last, m.Index - last) });
            }
            // Timestamp tint — go explicit (not via theme resource)
            // because the Application.Resources brushes can pick up
            // the wrong palette when ThemeHelper overrides at the
            // root-element level (the override is per-FrameworkElement,
            // not global, so Application.Resources still resolves to
            // the system theme). Same reason BuildSpeakerHeader picks
            // its colors explicitly above.
            var tsFg = IsAppDarkTheme()
                ? Microsoft.UI.ColorHelper.FromArgb(0xCC, 0xFF, 0xFF, 0xFF) // soft white
                : Microsoft.UI.ColorHelper.FromArgb(0xCC, 0x30, 0x30, 0x30); // soft dark grey
            p.Inlines.Add(new Run
            {
                Text = m.Value,
                FontSize = 10,
                FontFamily = new FontFamily("Consolas"),
                Foreground = new SolidColorBrush(tsFg),
            });
            last = m.Index + m.Length;
        }
        if (last < line.Length)
        {
            p.Inlines.Add(new Run { Text = line.Substring(last) });
        }
    }

    private static Brush ThemeBrush(string key)
    {
        if (Application.Current.Resources.TryGetValue(key, out var v) && v is Brush b)
            return b;
        return new SolidColorBrush(Microsoft.UI.Colors.Gray);
    }

    /// True iff the app is currently rendering with the Dark theme.
    /// Goes through `ThemeHelper.ResolvedIsDark()` because Dimmy's
    /// theme override stack reads UiPreferences first and the
    /// Windows registry as fallback — Application.Current.RequestedTheme
    /// only captures the system theme at process-start and does NOT
    /// honour the in-app override (Settings → Appearance). Every
    /// other theme-aware brush in the app routes through ThemeHelper;
    /// the transcript renderer must too.
    private static bool IsAppDarkTheme()
    {
        try
        {
            return ThemeHelper.ResolvedIsDark();
        }
        catch
        {
            return true; // Default to dark — was the only supported theme before this fix.
        }
    }
}

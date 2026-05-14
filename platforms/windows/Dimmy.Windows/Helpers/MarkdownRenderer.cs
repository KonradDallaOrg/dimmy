using System;
using System.Collections.Generic;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Documents;
using Microsoft.UI.Xaml.Media;
using Markdig;
using Markdig.Syntax;
using Markdig.Syntax.Inlines;

namespace Dimmy.Windows.Helpers;

/// Minimal Markdig → WinUI RichTextBlock renderer. Walks the Markdig
/// AST and emits Block / Inline elements that match the host's theme
/// resources, so Light/Dark switching just works.
///
/// Supported: paragraphs, headings (h1-h3), bullet & numbered lists,
/// blockquotes, code spans, **bold**, *italic*, links. Tables, nested
/// lists, fenced code blocks render as plain paragraphs (good-enough
/// for meeting recaps which never contain code).
public static class MarkdownRenderer
{
    private static readonly MarkdownPipeline Pipeline =
        new MarkdownPipelineBuilder().UseSoftlineBreakAsHardlineBreak().Build();

    /// Tracks the ActualTheme of the host RichTextBlock during a
    /// Render() pass so deeply-nested inline builders (code spans,
    /// quotes) can pick a theme-correct fixed color without having
    /// to thread the host parameter through every helper.
    /// Reset to Default after Render() so a follow-up call into a
    /// differently-themed host gets the right value on next entry.
    [ThreadStatic]
    private static ElementTheme _currentRenderTheme;

    /// Replace the contents of `target` with rendered markdown.
    public static void Render(RichTextBlock target, string markdown)
    {
        target.Blocks.Clear();
        if (string.IsNullOrWhiteSpace(markdown)) return;

        // Capture the visible theme of the host once, so inline
        // builders can pick brushes that contrast against the actual
        // rendered surface (acrylic-on-light vs acrylic-on-dark).
        // ActualTheme reflects what the user SEES, not what
        // Application.Current is set to (which can drift).
        _currentRenderTheme = target.ActualTheme;
        try
        {
            var doc = Markdown.Parse(markdown, Pipeline);
            foreach (var node in doc)
            {
                var block = ConvertBlock(node);
                if (block != null) target.Blocks.Add(block);
            }
        }
        catch (Exception ex)
        {
            // Fail-safe: stuff the raw text in a single paragraph.
            App.Log($"markdown render exc: {ex.Message}", "MD");
            var p = new Paragraph();
            p.Inlines.Add(new Run { Text = markdown });
            target.Blocks.Add(p);
        }
        finally
        {
            _currentRenderTheme = ElementTheme.Default;
        }
    }

    private static Microsoft.UI.Xaml.Documents.Block? ConvertBlock(Markdig.Syntax.Block node)
    {
        switch (node)
        {
            case HeadingBlock h:
                return BuildHeading(h);
            case ParagraphBlock p:
                return BuildParagraph(p.Inline);
            case ListBlock list:
                return BuildList(list);
            case QuoteBlock q:
                return BuildQuote(q);
            case ThematicBreakBlock _:
                return BuildHorizontalRule();
            default:
                // Fallback: render unknown block types as plain paragraph
                // using the raw text (lossy but never crashes).
                var raw = new Paragraph();
                raw.Inlines.Add(new Run { Text = node.ToString() ?? "" });
                return raw;
        }
    }

    private static Paragraph BuildHeading(HeadingBlock h)
    {
        var p = new Paragraph();
        // Headings get a tiny top margin and a bigger / heavier font.
        // Recap-style headings are usually h2/h3 — h1 is reserved for
        // section titles which we already render as card headers in XAML.
        double size = h.Level switch
        {
            1 => 18,
            2 => 16,
            3 => 14,
            _ => 13,
        };
        var bold = new Bold();
        AppendInlines(bold.Inlines, h.Inline);
        var run = new Span();
        run.Inlines.Add(bold);
        p.Inlines.Add(run);
        p.FontSize = size;
        p.Margin = new Thickness(0, 6, 0, 2);
        return p;
    }

    private static Paragraph BuildParagraph(ContainerInline? inline)
    {
        var p = new Paragraph { Margin = new Thickness(0, 0, 0, 6) };
        AppendInlines(p.Inlines, inline);
        return p;
    }

    private static Paragraph BuildList(ListBlock list)
    {
        // WinUI's RichTextBlock has no native list — emit a paragraph
        // with one line per item, prefixed with • or "1." Wrapping is
        // handled by RichTextBlock's TextWrapping property on the host.
        var p = new Paragraph { Margin = new Thickness(0, 0, 0, 6) };
        int idx = 1;
        bool ordered = list.IsOrdered;
        bool first = true;
        foreach (var item in list)
        {
            if (item is not ListItemBlock li) continue;
            if (!first) p.Inlines.Add(new LineBreak());
            first = false;

            var prefix = ordered ? $"{idx}.  " : "•  ";
            p.Inlines.Add(new Run
            {
                Text = prefix,
                Foreground = ThemeBrush("TextFillColorSecondaryBrush"),
            });
            // A ListItemBlock contains its own block children — usually
            // a single ParagraphBlock. Flatten to inlines.
            foreach (var child in li)
            {
                if (child is ParagraphBlock cp)
                {
                    AppendInlines(p.Inlines, cp.Inline);
                }
                else if (child is ListBlock nested)
                {
                    // Nested list: indent + recurse on a single line.
                    p.Inlines.Add(new LineBreak());
                    foreach (var sub in nested)
                    {
                        if (sub is ListItemBlock sli)
                        {
                            p.Inlines.Add(new Run
                            {
                                Text = "    ◦  ",
                                Foreground = ThemeBrush("TextFillColorSecondaryBrush"),
                            });
                            foreach (var subChild in sli)
                            {
                                if (subChild is ParagraphBlock scp)
                                    AppendInlines(p.Inlines, scp.Inline);
                            }
                            p.Inlines.Add(new LineBreak());
                        }
                    }
                }
            }
            idx++;
        }
        return p;
    }

    private static Paragraph BuildQuote(QuoteBlock q)
    {
        // Render quotes as italic + secondary color. WinUI Paragraph
        // doesn't support a left-border; the visual cue is the muted
        // italic styling.
        var p = new Paragraph
        {
            Margin = new Thickness(8, 4, 0, 6),
            FontStyle = global::Windows.UI.Text.FontStyle.Italic,
            Foreground = ThemeBrush("TextFillColorSecondaryBrush"),
        };
        bool first = true;
        foreach (var child in q)
        {
            if (child is ParagraphBlock cp)
            {
                if (!first) p.Inlines.Add(new LineBreak());
                first = false;
                AppendInlines(p.Inlines, cp.Inline);
            }
        }
        return p;
    }

    private static Paragraph BuildHorizontalRule()
    {
        var p = new Paragraph { Margin = new Thickness(0, 4, 0, 4) };
        p.Inlines.Add(new Run
        {
            Text = "──────────",
            Foreground = ThemeBrush("ControlStrokeColorDefaultBrush"),
        });
        return p;
    }

    private static void AppendInlines(InlineCollection sink, ContainerInline? container)
    {
        if (container == null) return;
        foreach (var inline in container)
        {
            var conv = ConvertInline(inline);
            if (conv != null) sink.Add(conv);
        }
    }

    private static Microsoft.UI.Xaml.Documents.Inline? ConvertInline(Markdig.Syntax.Inlines.Inline inline)
    {
        switch (inline)
        {
            case LiteralInline lit:
                return new Run { Text = lit.Content.ToString() };
            case EmphasisInline emp:
                return BuildEmphasis(emp);
            case CodeInline code:
                // Code span as plain styled Run, no chip box. Monospace
                // font + a theme-aware purple: GitHub-dark
                // (#6F42C1) on Light cards, Tailwind-lavender (#C4B5FD)
                // on Dark cards. Each picked for clear contrast on
                // the actual rendered surface.
                {
                    bool dark = _currentRenderTheme == ElementTheme.Dark;
                    var color = dark
                        ? Microsoft.UI.ColorHelper.FromArgb(0xFF, 0xC4, 0xB5, 0xFD)
                        : Microsoft.UI.ColorHelper.FromArgb(0xFF, 0x6F, 0x42, 0xC1);
                    return new Run
                    {
                        Text = code.Content,
                        FontFamily = new FontFamily("Consolas"),
                        FontWeight = Microsoft.UI.Text.FontWeights.SemiBold,
                        Foreground = new SolidColorBrush(color),
                    };
                }
            case LineBreakInline _:
                return new LineBreak();
            case LinkInline link:
                return BuildLink(link);
            case AutolinkInline auto:
                {
                    var hl = new Hyperlink();
                    hl.Inlines.Add(new Run { Text = auto.Url });
                    if (Uri.TryCreate(auto.Url, UriKind.Absolute, out var uri))
                        hl.NavigateUri = uri;
                    return hl;
                }
            case ContainerInline container:
                {
                    // Unknown container — flatten its children into a span.
                    var span = new Span();
                    AppendInlines(span.Inlines, container);
                    return span;
                }
            default:
                return new Run { Text = inline.ToString() ?? "" };
        }
    }

    private static Microsoft.UI.Xaml.Documents.Inline BuildEmphasis(EmphasisInline emp)
    {
        var children = new Span();
        AppendInlines(children.Inlines, emp);
        // Markdig sets DelimiterCount: 2 = strong/bold (** or __),
        // 1 = emphasis/italic (* or _). Combo of bold+italic uses 3
        // and we render that as bold for simplicity.
        if (emp.DelimiterCount >= 2)
        {
            var b = new Bold();
            foreach (var c in CopyInlines(children.Inlines)) b.Inlines.Add(c);
            return b;
        }
        var i = new Italic();
        foreach (var c in CopyInlines(children.Inlines)) i.Inlines.Add(c);
        return i;
    }

    private static Hyperlink BuildLink(LinkInline link)
    {
        var hl = new Hyperlink();
        AppendInlines(hl.Inlines, link);
        if (!string.IsNullOrEmpty(link.Url) && Uri.TryCreate(link.Url, UriKind.Absolute, out var uri))
            hl.NavigateUri = uri;
        return hl;
    }

    /// Inlines can only have one parent; copy() so the same Run isn't
    /// inserted twice when we re-parent under Bold/Italic.
    private static IEnumerable<Microsoft.UI.Xaml.Documents.Inline> CopyInlines(InlineCollection src)
    {
        var copy = new List<Microsoft.UI.Xaml.Documents.Inline>();
        foreach (var inl in src) copy.Add(inl);
        // Detach in reverse so indices stay valid.
        for (int i = copy.Count - 1; i >= 0; i--) src.RemoveAt(i);
        return copy;
    }

    private static Brush ThemeBrush(string key)
    {
        if (Application.Current.Resources.TryGetValue(key, out var v) && v is Brush b)
            return b;
        return new SolidColorBrush(Microsoft.UI.Colors.Gray);
    }
}

using System;
using System.Collections.Generic;
using System.Linq;
using System.Text.Json;

namespace Dimmy.Windows.Helpers;

public enum LegalItemKind { Heading, Paragraph, TableHeader, TableRow, Code }

/// <summary>One line of the legal text window: a heading, a paragraph, one
/// table row (cells plus the column proportions shared by its table) or a
/// licence text.</summary>
public sealed record LegalItem(LegalItemKind Kind, string Text, int Level, IReadOnlyList<string> Cells,
    IReadOnlyList<double> Columns);

/// <summary>The core's legal blocks (<c>dimmy_legal_document</c>) as the flat
/// list the window draws: a table becomes its header plus one item per row,
/// sharing the column proportions. Pure, so the headless test project
/// covers it.</summary>
public static class LegalDocument
{
    private static readonly IReadOnlyList<string> NoCells = Array.Empty<string>();
    private static readonly IReadOnlyList<double> NoColumns = Array.Empty<double>();

    public static List<LegalItem> Parse(string json)
    {
        using var doc = JsonDocument.Parse(json);
        var items = new List<LegalItem>();
        foreach (var b in doc.RootElement.EnumerateArray())
        {
            switch (b.GetProperty("t").GetString())
            {
                case "heading":
                    items.Add(new LegalItem(LegalItemKind.Heading, Text(b), b.GetProperty("level").GetInt32(), NoCells, NoColumns));
                    break;
                case "paragraph":
                    items.Add(new LegalItem(LegalItemKind.Paragraph, Text(b), 0, NoCells, NoColumns));
                    break;
                case "code":
                    items.Add(new LegalItem(LegalItemKind.Code, Licence(Text(b)), 0, NoCells, NoColumns));
                    break;
                case "table":
                    var header = Strings(b.GetProperty("header"));
                    var rows = b.GetProperty("rows").EnumerateArray().Select(Strings).ToList();
                    var columns = Proportions(header, rows);
                    items.Add(new LegalItem(LegalItemKind.TableHeader, "", 0, header, columns));
                    items.AddRange(rows.Select(r => new LegalItem(LegalItemKind.TableRow, "", 0, r, columns)));
                    break;
            }
        }
        return items;
    }

    /// <summary>A licence text with each paragraph reflowed: the texts come
    /// hard-wrapped at ~80 columns, which in a wide window leaves a narrow
    /// column several times longer than it needs to be. A paragraph whose
    /// lines carry structure (a list item, an indented line, a rule of
    /// dashes) keeps its line breaks. Paragraphs stay apart by a blank line
    /// (runs of blank lines collapse to one).</summary>
    public static string Licence(string text)
    {
        var paragraphs = new List<string>();
        var para = new List<string>();
        void Flush()
        {
            if (para.Count > 0) paragraphs.Add(Reflow(para));
            para.Clear();
        }
        foreach (var line in text.Replace("\r\n", "\n").Split('\n'))
        {
            if (line.Trim().Length == 0) Flush();
            else para.Add(line);
        }
        Flush();
        return string.Join("\n\n", paragraphs);
    }

    private static readonly System.Text.RegularExpressions.Regex ListMarker =
        new(@"^(\(?[0-9a-zA-Z]{1,3}[.)]|[-*•+])\s");

    /// <summary>One paragraph as flowing text when it is prose; with its line
    /// breaks (common indent removed) when it has structure.</summary>
    public static string Reflow(IReadOnlyList<string> lines)
    {
        int cut = lines.Min(l => l.Length - l.TrimStart().Length);
        var body = lines.Select(l => l.Substring(cut).TrimEnd()).ToList();
        bool structured = body.Count > 1 && body.Skip(1).Any(l =>
            l.Length > 0 && (char.IsWhiteSpace(l[0]) || ListMarker.IsMatch(l) || !l.Any(char.IsLetterOrDigit)));
        structured |= body.Count > 1 && !body[0].Any(char.IsLetterOrDigit);
        return structured ? string.Join("\n", body) : string.Join(" ", body.Select(l => l.Trim()));
    }

    /// <summary>Star widths for a table's columns: each column's longest cell
    /// (header included), clamped so a long URL column can't squeeze a short
    /// version column to nothing.</summary>
    public static IReadOnlyList<double> Proportions(IReadOnlyList<string> header, IReadOnlyList<IReadOnlyList<string>> rows)
    {
        int n = header.Count;
        var widths = new double[n];
        for (int c = 0; c < n; c++)
        {
            int longest = header[c].Length;
            foreach (var r in rows)
                if (c < r.Count) longest = Math.Max(longest, r[c].Length);
            widths[c] = Math.Clamp(longest, 6, 48);
        }
        return widths;
    }

    private static string Text(JsonElement b) => b.GetProperty("text").GetString() ?? "";

    private static IReadOnlyList<string> Strings(JsonElement arr) =>
        arr.EnumerateArray().Select(e => e.GetString() ?? "").ToList();
}

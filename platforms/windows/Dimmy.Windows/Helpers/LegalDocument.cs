using System;
using System.Collections.Generic;
using System.Linq;
using System.Text.Json;

namespace Dimmy.Windows.Helpers;

public enum LegalItemKind { Heading, Paragraph, TableHeader, TableRow, Code }

/// <summary>One line of the legal text window: a heading, a paragraph, one
/// table row (cells plus the column proportions shared by its table) or a
/// licence text.</summary>
public sealed record LegalItem(LegalItemKind Kind, string Text, int Level, IReadOnlyList<string> Cells, IReadOnlyList<double> Columns);

/// <summary>The core's legal blocks (<c>dimmy_legal_document</c>) as the flat
/// list the window virtualizes: a table becomes its header row plus one item
/// per row, so a 660-row table costs only the rows on screen. Pure, so the
/// headless test project covers it.</summary>
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
                    items.Add(new LegalItem(LegalItemKind.Code, Text(b), 0, NoCells, NoColumns));
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

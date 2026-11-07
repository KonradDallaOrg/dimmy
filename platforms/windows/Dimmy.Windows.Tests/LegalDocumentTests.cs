using Dimmy.Windows.Helpers;
using Xunit;

namespace Dimmy.Windows.Tests;

/// The legal window draws a flat list built from the core's blocks. A table
/// turns into one header item and one item per row, all sharing the same
/// column proportions so the columns line up; a licence text keeps one card,
/// its hard-wrapped paragraphs reflowed to the window width.
public class LegalDocumentTests
{
    private const string Json = """
        [{"t":"heading","level":1,"text":"Open source notices"},
         {"t":"paragraph","text":"Dimmy is free software."},
         {"t":"table","header":["Crate","Version","Repository"],
          "rows":[["a","1.0","https://github.com/someone/a-very-long-repository-name-that-goes-on"],["bb","2.0.1","x"]]},
         {"t":"code","text":"MIT License\n\n\nCopyright (c) X"}]
        """;

    [Fact]
    public void Blocks_become_items_and_a_table_one_item_per_row()
    {
        var items = LegalDocument.Parse(Json);
        Assert.Equal(new[] { LegalItemKind.Heading, LegalItemKind.Paragraph, LegalItemKind.TableHeader,
            LegalItemKind.TableRow, LegalItemKind.TableRow, LegalItemKind.Code }, items.ConvertAll(i => i.Kind));
        Assert.Equal(1, items[0].Level);
        Assert.Equal("Open source notices", items[0].Text);
        Assert.Equal(new[] { "Crate", "Version", "Repository" }, items[2].Cells);
        Assert.Equal(new[] { "bb", "2.0.1", "x" }, items[4].Cells);
        Assert.Equal("MIT License\n\nCopyright (c) X", items[5].Text);
    }

    [Fact]
    public void Every_row_of_a_table_shares_its_column_proportions()
    {
        var items = LegalDocument.Parse(Json);
        Assert.Same(items[2].Columns, items[3].Columns);
        Assert.Same(items[2].Columns, items[4].Columns);
    }

    [Fact]
    public void Column_proportions_follow_the_content_within_bounds()
    {
        var items = LegalDocument.Parse(Json);
        var c = items[2].Columns;
        Assert.Equal(6, c[0]);   // "Crate" is 5: never narrower than 6
        Assert.Equal(7, c[1]);   // "Version"
        Assert.Equal(48, c[2]);  // a long URL is capped
    }

    [Fact]
    public void Hard_wrapped_prose_flows_to_the_window_width()
    {
        var text = "Permission is hereby granted, free of charge, to any person obtaining a copy\r\n" +
                   "of this software and associated documentation files (the \"Software\"), to deal\r\n" +
                   "in the Software without restriction.\r\n\r\nTHE SOFTWARE IS PROVIDED \"AS IS\".";
        Assert.Equal("Permission is hereby granted, free of charge, to any person obtaining a copy " +
            "of this software and associated documentation files (the \"Software\"), to deal " +
            "in the Software without restriction.\n\nTHE SOFTWARE IS PROVIDED \"AS IS\".", LegalDocument.Licence(text));
    }

    [Fact]
    public void Lists_indents_and_rules_keep_their_line_breaks()
    {
        Assert.Equal("Conditions:\n1. Keep the notice.\n2. Do not misrepresent.",
            LegalDocument.Licence("  Conditions:\n  1. Keep the notice.\n  2. Do not misrepresent."));
        Assert.Equal("Section 1\n    indented detail", LegalDocument.Licence("Section 1\n    indented detail"));
        Assert.Equal("=========\nTitle\n=========", LegalDocument.Licence("=========\nTitle\n========="));
    }
}

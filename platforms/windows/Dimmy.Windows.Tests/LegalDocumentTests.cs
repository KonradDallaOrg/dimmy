using Dimmy.Windows.Helpers;
using Xunit;

namespace Dimmy.Windows.Tests;

/// The legal window draws a flat, virtualized list built from the core's
/// blocks. A table must turn into one header item and one item per row,
/// all sharing the same column proportions so the columns line up.
public class LegalDocumentTests
{
    private const string Json = """
        [{"t":"heading","level":1,"text":"Open source notices"},
         {"t":"paragraph","text":"Dimmy is free software."},
         {"t":"table","header":["Crate","Version","Repository"],
          "rows":[["a","1.0","https://github.com/someone/a-very-long-repository-name-that-goes-on"],["bb","2.0.1","x"]]},
         {"t":"code","text":"MIT License\n\nCopyright (c) X"}]
        """;

    [Fact]
    public void Blocks_become_one_item_each_and_a_table_one_item_per_row()
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
}

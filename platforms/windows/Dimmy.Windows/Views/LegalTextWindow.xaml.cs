using System.Collections.Generic;
using Dimmy.Windows.Helpers;
using Dimmy.Windows.Interop;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace Dimmy.Windows.Views;

/// <summary>The licence or the open source notices: headings, paragraphs,
/// tables with aligned columns, licence texts in cards. A normal window, so
/// it resizes and scrolls; the blocks come parsed from the core
/// (<c>dimmy_legal_document</c>). Why it is not virtualized: see the
/// XAML.</summary>
public sealed partial class LegalTextWindow : Window
{
    private static readonly Dictionary<int, LegalTextWindow> Open = new();

    /// <summary>kind 0 licence, 1 open source notices. One window per kind:
    /// asking again brings it back to the front.</summary>
    public static void ShowFor(int kind, string title)
    {
        if (Open.TryGetValue(kind, out var existing))
        {
            existing.Activate();
            return;
        }
        var json = DimmyNative.LegalDocument(kind);
        var items = string.IsNullOrEmpty(json) ? new List<LegalItem>() : LegalDocument.Parse(json);
        if (items.Count == 0)
            items.Add(new LegalItem(LegalItemKind.Paragraph, "Text unavailable.", 0,
                System.Array.Empty<string>(), System.Array.Empty<double>()));
        var window = new LegalTextWindow(title, items);
        Open[kind] = window;
        window.Closed += (_, _) => Open.Remove(kind);
        window.Activate();
    }

    private LegalTextWindow(string title, List<LegalItem> items)
    {
        InitializeComponent();
        Title = title;
        WindowHelper.SetDimmyIcon(this);
        WindowHelper.ResizeLogical(this, 860, 720);
        Root.Background = ThemeHelper.ResolvedPageBackground();
        Root.RequestedTheme = ThemeHelper.ResolvedElementTheme();
        foreach (var item in items)
            Body.Children.Add(Build(item));
    }

    private static Style S(string key) => (Style)Application.Current.Resources[key];

    private static TextBlock Text(string text, string style) => new()
    {
        Text = text,
        Style = S(style),
        TextWrapping = TextWrapping.Wrap,
        IsTextSelectionEnabled = true,
    };

    private static UIElement Build(LegalItem item)
    {
        switch (item.Kind)
        {
            case LegalItemKind.Heading:
                var h = Text(item.Text, item.Level <= 1 ? "DimmyPageTitle" : item.Level == 2 ? "DimmyHeadline" : "DimmySection");
                h.Margin = new Thickness(0, item.Level <= 1 ? 0 : 20, 0, 6);
                return h;
            case LegalItemKind.Paragraph:
                var p = Text(item.Text, "DimmyBody");
                p.Margin = new Thickness(0, 2, 0, 8);
                return p;
            case LegalItemKind.Code:
                return new Border
                {
                    Style = S("DimmyCard"),
                    Margin = new Thickness(0, 4, 0, 10),
                    Child = Text(item.Text, "DimmyBody"),
                };
            default:
                return Row(item);
        }
    }

    /// <summary>One table row. Every row of a table gets the same star
    /// proportions, so the columns line up down the whole table.</summary>
    private static UIElement Row(LegalItem item)
    {
        bool header = item.Kind == LegalItemKind.TableHeader;
        var grid = new Grid { ColumnSpacing = 16, Padding = new Thickness(4, header ? 8 : 5, 4, header ? 8 : 5) };
        for (int c = 0; c < item.Cells.Count; c++)
        {
            double star = c < item.Columns.Count ? item.Columns[c] : 1;
            grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(star, GridUnitType.Star) });
            var cell = Text(item.Cells[c], header ? "DimmyCardTitle" : "DimmyBody");
            Grid.SetColumn(cell, c);
            grid.Children.Add(cell);
        }
        var stack = new StackPanel();
        stack.Children.Add(grid);
        stack.Children.Add(new Border { Style = S("DimmyDivider") });
        return stack;
    }
}

using System.Collections.Generic;
using Dimmy.Windows.Helpers;
using Dimmy.Windows.Interop;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace Dimmy.Windows.Views;

/// <summary>The licence or the open source notices, laid out: headings,
/// paragraphs, tables with aligned columns, licence texts in monospace cards.
/// A normal window, so it resizes and scrolls; the blocks come parsed from
/// the core (<c>dimmy_legal_document</c>), and an ItemsRepeater builds only
/// what is on screen: the notices are ~1 MB of text.</summary>
public sealed class LegalTextWindow : Window
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
        Title = title;
        WindowHelper.SetDimmyIcon(this);
        WindowHelper.ResizeLogical(this, 900, 720);

        var repeater = new ItemsRepeater
        {
            ItemsSource = items,
            ItemTemplate = new LegalElementFactory(),
            Layout = new StackLayout { Spacing = 0 },
        };
        var scroll = new ScrollViewer
        {
            Content = repeater,
            Padding = new Thickness(28, 20, 28, 24),
            VerticalScrollBarVisibility = ScrollBarVisibility.Visible,
            HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled,
        };
        Content = new Grid
        {
            Background = ThemeHelper.ResolvedPageBackground(),
            RequestedTheme = ThemeHelper.ResolvedElementTheme(),
            Children = { scroll },
        };
    }

    /// <summary>Builds the element for one item when the repeater needs it.</summary>
    private sealed class LegalElementFactory : IElementFactory
    {
        public UIElement GetElement(ElementFactoryGetArgs args) =>
            args.Data is LegalItem item ? Build(item) : new Grid();

        public void RecycleElement(ElementFactoryRecycleArgs args) { }

        private static Style S(string key) => (Style)Application.Current.Resources[key];

        private static UIElement Build(LegalItem item)
        {
            switch (item.Kind)
            {
                case LegalItemKind.Heading:
                    return new TextBlock
                    {
                        Text = item.Text,
                        Style = S(item.Level <= 1 ? "DimmyPageTitle" : item.Level == 2 ? "DimmyHeadline" : "DimmySection"),
                        TextWrapping = TextWrapping.Wrap,
                        IsTextSelectionEnabled = true,
                        Margin = new Thickness(0, item.Level <= 1 ? 0 : 20, 0, 6),
                    };
                case LegalItemKind.Paragraph:
                    return new TextBlock
                    {
                        Text = item.Text,
                        Style = S("DimmyBody"),
                        TextWrapping = TextWrapping.Wrap,
                        IsTextSelectionEnabled = true,
                        Margin = new Thickness(0, 2, 0, 8),
                    };
                case LegalItemKind.Code:
                    return new Border
                    {
                        Style = S("DimmyCard"),
                        Margin = new Thickness(0, 4, 0, 10),
                        Child = new TextBlock
                        {
                            Text = item.Text,
                            Style = S("DimmyCaption"),
                            FontFamily = new FontFamily("Consolas"),
                            TextWrapping = TextWrapping.Wrap,
                            IsTextSelectionEnabled = true,
                        },
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
                var cell = new TextBlock
                {
                    Text = item.Cells[c],
                    Style = S(header ? "DimmyCardTitle" : "DimmyBody"),
                    TextWrapping = TextWrapping.Wrap,
                    IsTextSelectionEnabled = true,
                };
                Grid.SetColumn(cell, c);
                grid.Children.Add(cell);
            }
            var stack = new StackPanel();
            stack.Children.Add(grid);
            stack.Children.Add(new Border { Style = S("DimmyDivider") });
            return stack;
        }
    }
}

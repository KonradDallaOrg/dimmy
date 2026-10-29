using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Dimmy.Windows.Helpers;
using Dimmy.Windows.Services;

namespace Dimmy.Windows.Views;

/// <summary>
/// Asks once whether Dimmy may send usage statistics and crash reports.
/// The two answers look the same on purpose: consent that is nudged is not
/// consent. Closing without answering sends nothing and asks again next
/// launch.
/// </summary>
public sealed class StatsConsentWindow : Window
{
    public StatsConsentWindow()
    {
        Title = "Help improve Dimmy";

        var title = new TextBlock
        {
            Text = "Help improve Dimmy",
            FontSize = 18,
            FontWeight = Microsoft.UI.Text.FontWeights.SemiBold,
        };
        var body = new TextBlock
        {
            Text = "Can Dimmy send anonymous usage statistics and crash reports? They tell us which features are used and what breaks. Never your audio, your text, your prompts, file names or keys. You can change this any time in Settings, Privacy.",
            TextWrapping = TextWrapping.Wrap,
        };
        var yes = new Button { Content = "Yes, send them", HorizontalAlignment = HorizontalAlignment.Stretch };
        var no = new Button { Content = "No, thanks", HorizontalAlignment = HorizontalAlignment.Stretch };
        yes.SetValue(Microsoft.UI.Xaml.Automation.AutomationProperties.AutomationIdProperty, "StatsConsentYes");
        no.SetValue(Microsoft.UI.Xaml.Automation.AutomationProperties.AutomationIdProperty, "StatsConsentNo");
        yes.Click += (_, _) => { StatsConsent.Answer(true); Close(); };
        no.Click += (_, _) => { StatsConsent.Answer(false); Close(); };

        var buttons = new Grid { ColumnSpacing = 12 };
        buttons.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        buttons.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        Grid.SetColumn(no, 1);
        buttons.Children.Add(yes);
        buttons.Children.Add(no);

        var panel = new StackPanel { Spacing = 14, Padding = new Thickness(28, 24, 28, 24) };
        panel.Children.Add(title);
        panel.Children.Add(body);
        panel.Children.Add(buttons);

        var root = new Grid { RequestedTheme = ThemeHelper.ResolvedElementTheme() };
        if (Application.Current.Resources.TryGetValue("ApplicationPageBackgroundThemeBrush", out var bg)
            && bg is Microsoft.UI.Xaml.Media.Brush brush)
            root.Background = brush;
        root.Children.Add(panel);
        Content = root;

        WindowHelper.ResizeLogical(this, 480, 260);
    }
}

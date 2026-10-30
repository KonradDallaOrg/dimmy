using System;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Dimmy.Windows.Helpers;
using Dimmy.Windows.Services;

namespace Dimmy.Windows.Views;

/// <summary>
/// One-off notice for installs that predate the terms (or whose terms
/// version moved on): checkbox plus Accept. Built in code, no XAML.
/// Deliberately light: Dimmy keeps working while it is open or ignored, and
/// closing it without accepting just means it shows again next launch.
/// </summary>
public sealed class TermsWindow : Window
{
    public TermsWindow()
    {
        Title = "Dimmy terms";

        var intro = new TextBlock
        {
            Text = "Dimmy has Terms of Service and an End-User Licence Agreement. Please read and accept them.",
            TextWrapping = TextWrapping.Wrap,
        };
        var link = new HyperlinkButton
        {
            Content = "Read the terms",
            NavigateUri = new Uri(TermsAcceptance.Url),
            Padding = new Thickness(0),
        };
        var check = new CheckBox
        {
            Content = "I accept the Terms of Service and End-User Licence Agreement",
        };
        check.SetValue(
            Microsoft.UI.Xaml.Automation.AutomationProperties.AutomationIdProperty,
            "TermsLaunchCheckBox");
        var accept = new Button
        {
            Content = "Accept",
            IsEnabled = false,
            HorizontalAlignment = HorizontalAlignment.Right,
        };
        if (Application.Current.Resources.TryGetValue("AccentButtonStyle", out var accentStyle)
            && accentStyle is Style style)
            accept.Style = style;
        check.Checked += (_, _) => accept.IsEnabled = true;
        check.Unchecked += (_, _) => accept.IsEnabled = false;
        accept.Click += (_, _) =>
        {
            if (check.IsChecked != true) return;
            TermsAcceptance.Accept();
            Close();
        };

        var panel = new StackPanel { Spacing = 14, Padding = new Thickness(28, 24, 28, 24) };
        panel.Children.Add(intro);
        panel.Children.Add(link);
        panel.Children.Add(check);
        panel.Children.Add(accept);

        var root = new Grid { RequestedTheme = ThemeHelper.ResolvedElementTheme() };
        if (Application.Current.Resources.TryGetValue("ApplicationPageBackgroundThemeBrush", out var bg)
            && bg is Microsoft.UI.Xaml.Media.Brush brush)
            root.Background = brush;
        root.Children.Add(panel);
        Content = root;

        WindowHelper.ResizeLogical(this, 480, 280);

        try
        {
            var iconPath = System.IO.Path.Combine(AppContext.BaseDirectory, "Assets", "dimmy.ico");
            if (System.IO.File.Exists(iconPath))
                WindowHelper.GetAppWindow(this)?.SetIcon(iconPath);
        }
        catch { }
    }
}

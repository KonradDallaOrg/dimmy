using System;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Dimmy.Windows.Helpers;
using Dimmy.Windows.Interop;
using Dimmy.Windows.Services;

namespace Dimmy.Windows.Views;

/// <summary>
/// Asks once, after a native crash, whether to send that one crash report.
/// Only for users with crash reports off; with them on the core sends it
/// silently and this never opens. Wording and state live in the core
/// (<c>telemetry/crash_prompt.rs</c>) so macOS asks the same question.
/// </summary>
public sealed class CrashReportWindow : Window
{
    private bool _answered;

    public CrashReportWindow()
    {
        var lang = System.Globalization.CultureInfo.CurrentUICulture.TwoLetterISOLanguageName;
        string T(string kind, string fallback) => DimmyNative.CrashReportText(kind, lang) ?? fallback;

        var titleText = T("title", "Dimmy closed unexpectedly");
        Title = titleText;

        var title = new TextBlock
        {
            Text = titleText,
            FontSize = 18,
            FontWeight = Microsoft.UI.Text.FontWeights.SemiBold,
        };
        var body = new TextBlock
        {
            Text = T("body", "Dimmy closed unexpectedly last time. Send the crash report so we can fix it?"),
            TextWrapping = TextWrapping.Wrap,
        };
        var always = new CheckBox { Content = T("always", "Always send crash reports") };
        var send = new Button { Content = T("send", "Send"), HorizontalAlignment = HorizontalAlignment.Stretch };
        var dontSend = new Button { Content = T("dont_send", "Don't send"), HorizontalAlignment = HorizontalAlignment.Stretch };
        send.SetValue(Microsoft.UI.Xaml.Automation.AutomationProperties.AutomationIdProperty, "CrashReportSend");
        dontSend.SetValue(Microsoft.UI.Xaml.Automation.AutomationProperties.AutomationIdProperty, "CrashReportDontSend");
        send.Click += (_, _) => Answer(new CrashReportChoice(true, always.IsChecked == true));
        dontSend.Click += (_, _) => Answer(new CrashReportChoice(false, always.IsChecked == true));
        Closed += (_, _) =>
        {
            if (!_answered) Resolve(new CrashReportChoice(false, false));
        };

        var buttons = new Grid { ColumnSpacing = 12 };
        buttons.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        buttons.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        Grid.SetColumn(dontSend, 1);
        buttons.Children.Add(send);
        buttons.Children.Add(dontSend);

        var panel = new StackPanel { Spacing = 14, Padding = new Thickness(28, 24, 28, 24) };
        panel.Children.Add(title);
        panel.Children.Add(body);
        panel.Children.Add(always);
        panel.Children.Add(buttons);

        var root = new Grid
        {
            RequestedTheme = ThemeHelper.ResolvedElementTheme(),
            Background = ThemeHelper.ResolvedPageBackground(),
        };
        root.Children.Add(panel);
        Content = root;

        WindowHelper.ResizeLogical(this, 480, 250);
        WindowHelper.SetDimmyIcon(this);
    }

    private void Answer(CrashReportChoice choice)
    {
        Resolve(choice);
        Close();
    }

    private void Resolve(CrashReportChoice choice)
    {
        _answered = true;
        try
        {
            if (choice.AlwaysSend) DimmyNative.CrashReportsEnabled = true;
            var rc = DimmyNative.dimmy_crash_report_resolve(choice.ResolveArgument);
            App.Log($"crash report answer: send={choice.Send} always={choice.AlwaysSend} rc={rc}", "Telemetry");
        }
        catch (Exception ex)
        {
            App.Log($"crash report answer failed: {ex.Message}", "Telemetry");
        }
    }
}

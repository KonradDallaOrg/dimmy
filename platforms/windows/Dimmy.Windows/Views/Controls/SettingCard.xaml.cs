using System;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Markup;

namespace Dimmy.Windows.Views.Controls;

/// <summary>
/// Win11 Settings card — `.scard` pattern from the design bundle.
/// One card per setting: icon + label + description + control on the right.
///
/// Use <see cref="Glyph"/> for the Segoe Fluent codepoint, <see cref="Label"/>
/// for the primary line, <see cref="Description"/> for the secondary line,
/// and <see cref="Control"/> (the default Content slot) for the right-side
/// element (Toggle / ComboBox / Button / etc.).
/// </summary>
[ContentProperty(Name = "Control")]
public sealed partial class SettingCard : UserControl
{
    public SettingCard()
    {
        this.InitializeComponent();
    }

    public static readonly DependencyProperty GlyphProperty =
        DependencyProperty.Register(nameof(Glyph), typeof(string), typeof(SettingCard),
            new PropertyMetadata(string.Empty, OnGlyphChanged));

    public string Glyph
    {
        get => (string)GetValue(GlyphProperty);
        set => SetValue(GlyphProperty, value);
    }

    private static void OnGlyphChanged(DependencyObject d, DependencyPropertyChangedEventArgs e)
    {
        if (d is SettingCard card)
        {
            var glyph = e.NewValue as string ?? string.Empty;
            card.GlyphIcon.Glyph = glyph;
            card.GlyphIcon.Visibility = string.IsNullOrEmpty(glyph)
                ? Visibility.Collapsed : Visibility.Visible;
        }
    }

    public static readonly DependencyProperty LabelProperty =
        DependencyProperty.Register(nameof(Label), typeof(string), typeof(SettingCard),
            new PropertyMetadata(string.Empty, OnLabelChanged));

    public string Label
    {
        get => (string)GetValue(LabelProperty);
        set => SetValue(LabelProperty, value);
    }

    private static void OnLabelChanged(DependencyObject d, DependencyPropertyChangedEventArgs e)
    {
        if (d is SettingCard card)
        {
            card.LabelText.Text = e.NewValue as string ?? string.Empty;
        }
    }

    public static readonly DependencyProperty DescriptionProperty =
        DependencyProperty.Register(nameof(Description), typeof(string), typeof(SettingCard),
            new PropertyMetadata(string.Empty, OnDescriptionChanged));

    public string Description
    {
        get => (string)GetValue(DescriptionProperty);
        set => SetValue(DescriptionProperty, value);
    }

    private static void OnDescriptionChanged(DependencyObject d, DependencyPropertyChangedEventArgs e)
    {
        if (d is SettingCard card)
        {
            var desc = e.NewValue as string ?? string.Empty;
            card.DescriptionText.Text = desc;
            card.DescriptionText.Visibility = string.IsNullOrEmpty(desc)
                ? Visibility.Collapsed : Visibility.Visible;
        }
    }

    public static readonly DependencyProperty InfoTipProperty =
        DependencyProperty.Register(nameof(InfoTip), typeof(string), typeof(SettingCard),
            new PropertyMetadata(string.Empty, OnInfoTipChanged));

    /// <summary>Longer "why / how" text revealed behind the ⓘ icon — shown on
    /// hover (tooltip) and click (flyout). Empty hides the icon.</summary>
    public string InfoTip
    {
        get => (string)GetValue(InfoTipProperty);
        set => SetValue(InfoTipProperty, value);
    }

    private static void OnInfoTipChanged(DependencyObject d, DependencyPropertyChangedEventArgs e)
    {
        if (d is SettingCard card)
        {
            var tip = e.NewValue as string ?? string.Empty;
            card.InfoFlyoutText.Text = tip;
            ToolTipService.SetToolTip(card.InfoButton, string.IsNullOrEmpty(tip) ? null : tip);
            card._hasTip = !string.IsNullOrEmpty(tip);
            card.UpdateInfoButton();
        }
    }

    public static readonly DependencyProperty HelpUrlProperty =
        DependencyProperty.Register(nameof(HelpUrl), typeof(string), typeof(SettingCard),
            new PropertyMetadata(string.Empty, OnHelpUrlChanged));

    /// <summary>"Open full guide →" link shown INSIDE the ⓘ click-card. The
    /// browser opens only when the user clicks that link. Empty hides it.</summary>
    public string HelpUrl
    {
        get => (string)GetValue(HelpUrlProperty);
        set => SetValue(HelpUrlProperty, value);
    }

    private bool _hasTip;
    private bool _hasLink;

    private static void OnHelpUrlChanged(DependencyObject d, DependencyPropertyChangedEventArgs e)
    {
        if (d is SettingCard card)
        {
            var url = e.NewValue as string ?? string.Empty;
            if (!string.IsNullOrEmpty(url) && Uri.TryCreate(url, UriKind.Absolute, out var uri))
            {
                card.InfoFlyoutLink.NavigateUri = uri;
                card.InfoFlyoutLink.Visibility = Visibility.Visible;
                card._hasLink = true;
            }
            else
            {
                card.InfoFlyoutLink.Visibility = Visibility.Collapsed;
                card._hasLink = false;
            }
            card.UpdateInfoButton();
        }
    }

    /// <summary>Show the ⓘ when there's anything behind it (hover/click text or
    /// a guide link).</summary>
    private void UpdateInfoButton()
    {
        InfoButton.Visibility = (_hasTip || _hasLink) ? Visibility.Visible : Visibility.Collapsed;
    }

    public static readonly DependencyProperty ControlProperty =
        DependencyProperty.Register(nameof(Control), typeof(object), typeof(SettingCard),
            new PropertyMetadata(null, OnControlChanged));

    public object? Control
    {
        get => GetValue(ControlProperty);
        set => SetValue(ControlProperty, value);
    }

    private static void OnControlChanged(DependencyObject d, DependencyPropertyChangedEventArgs e)
    {
        if (d is SettingCard card)
        {
            card.ControlPresenter.Content = e.NewValue;
        }
    }
}

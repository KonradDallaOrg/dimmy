using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Markup;

namespace Dimmy.Windows.Views.Controls;

/// <summary>
/// Win11 Settings card — `.scard` pattern from the design bundle.
/// One card per setting: icon + label + (i) info button + short description
/// + control on the right.
///
/// Use <see cref="Glyph"/> for the Segoe Fluent codepoint, <see cref="Label"/>
/// for the primary line, <see cref="Description"/> for the (short) secondary
/// line, <see cref="Hint"/> for the long-form explanation behind the (i)
/// button, and <see cref="Control"/> (the default Content slot) for the
/// right-side element (Toggle / ComboBox / Button / etc.).
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

    public static readonly DependencyProperty HintProperty =
        DependencyProperty.Register(nameof(Hint), typeof(string), typeof(SettingCard),
            new PropertyMetadata(string.Empty, OnHintChanged));

    /// <summary>
    /// Long-form explanation surfaced behind the inline (i) button. Hidden
    /// when empty. The same string is wired up as the hover tooltip on the
    /// icon so users who never click still see the copy. Mirrors the macOS
    /// <c>MacGroupFooter</c> / <c>MacNote</c> off-row pattern.
    /// </summary>
    public string Hint
    {
        get => (string)GetValue(HintProperty);
        set => SetValue(HintProperty, value);
    }

    private static void OnHintChanged(DependencyObject d, DependencyPropertyChangedEventArgs e)
    {
        if (d is SettingCard card)
        {
            var hint = e.NewValue as string ?? string.Empty;
            card.HintFlyoutText.Text = hint;
            ToolTipService.SetToolTip(card.HintButton,
                string.IsNullOrEmpty(hint) ? null : hint);
            card.HintButton.Visibility = string.IsNullOrEmpty(hint)
                ? Visibility.Collapsed : Visibility.Visible;
        }
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

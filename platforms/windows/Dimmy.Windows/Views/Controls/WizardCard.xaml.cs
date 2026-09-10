using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace Dimmy.Windows.Views.Controls;

/// <summary>A clickable card: icon above, title, one line of description.
///
/// <para>Exists because Windows had no such control — <see cref="SettingCard"/>
/// is a ROW (glyph left, control right), and the onboarding wrote its own
/// cards inline as Borders inside its own XAML. Three of these sit side by
/// side to launch the setup wizards, and the Mac already has the equivalent in
/// MacTile + MacRow, so packaging it keeps the two platforms recognisably the
/// same screen rather than two different ideas.</para></summary>
public sealed partial class WizardCard : UserControl
{
    public WizardCard() => InitializeComponent();

    public static readonly DependencyProperty GlyphProperty =
        DependencyProperty.Register(nameof(Glyph), typeof(string),
            typeof(WizardCard), new PropertyMetadata("", OnGlyphChanged));

    /// <summary>A Segoe Fluent Icons code point, e.g. <c>&amp;#xE8AB;</c>.</summary>
    public string Glyph
    {
        get => (string)GetValue(GlyphProperty);
        set => SetValue(GlyphProperty, value);
    }

    public static readonly DependencyProperty TitleProperty =
        DependencyProperty.Register(nameof(Title), typeof(string),
            typeof(WizardCard), new PropertyMetadata("", OnTitleChanged));

    public string Title
    {
        get => (string)GetValue(TitleProperty);
        set => SetValue(TitleProperty, value);
    }

    public static readonly DependencyProperty DescriptionProperty =
        DependencyProperty.Register(nameof(Description), typeof(string),
            typeof(WizardCard), new PropertyMetadata("", OnDescriptionChanged));

    public string Description
    {
        get => (string)GetValue(DescriptionProperty);
        set => SetValue(DescriptionProperty, value);
    }

    public event RoutedEventHandler? Click;

    private static void OnGlyphChanged(DependencyObject d, DependencyPropertyChangedEventArgs e)
    {
        if (d is WizardCard c) c.Icon.Glyph = (string)e.NewValue;
    }

    private static void OnTitleChanged(DependencyObject d, DependencyPropertyChangedEventArgs e)
    {
        if (d is not WizardCard c) return;
        var text = (string)e.NewValue;
        c.TitleText.Text = text;
        // The card's accessible name: without this a screen reader announces
        // the button as unlabelled, since the text lives in child TextBlocks.
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(c.Root, text);
    }

    private static void OnDescriptionChanged(DependencyObject d, DependencyPropertyChangedEventArgs e)
    {
        if (d is WizardCard c) c.DescriptionText.Text = (string)e.NewValue;
    }

    private void Root_Click(object sender, RoutedEventArgs e) => Click?.Invoke(this, e);
}

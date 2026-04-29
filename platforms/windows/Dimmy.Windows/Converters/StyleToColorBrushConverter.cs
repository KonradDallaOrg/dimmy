using System;
using Microsoft.UI;
using Microsoft.UI.Xaml.Data;
using Microsoft.UI.Xaml.Media;
using Windows.UI;

namespace Dimmy.Windows.Converters;

/// <summary>Maps an LLM style key (e.g. "professional", "genz") to the
/// brand-ish color we show as a swatch in the Output → Style picker.
/// Keeps the picker visually parseable: each style gets a recognisable
/// dot, so users don't have to read every label to scan options.</summary>
public sealed class StyleToColorBrushConverter : IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, string language)
    {
        var key = (value as string ?? string.Empty).ToLowerInvariant();
        var c = key switch
        {
            "off" => Color.FromArgb(0xFF, 0x9E, 0x9E, 0x9E),            // grey
            "correct" => Color.FromArgb(0xFF, 0x42, 0xA5, 0xF5),         // blue
            "summarize" => Color.FromArgb(0xFF, 0x26, 0xC6, 0xDA),       // cyan
            "elaborate" => Color.FromArgb(0xFF, 0x66, 0xBB, 0x6A),       // green
            "comprehensible" => Color.FromArgb(0xFF, 0xAB, 0x47, 0xBC),  // purple
            "professional" => Color.FromArgb(0xFF, 0x1E, 0x88, 0xE5),    // deep blue
            "prompt" => Color.FromArgb(0xFF, 0x7E, 0x57, 0xC2),          // indigo
            "genz" => Color.FromArgb(0xFF, 0xEC, 0x40, 0x7A),            // hot pink
            "boomer" => Color.FromArgb(0xFF, 0x8D, 0x6E, 0x63),          // brown
            "emoji" => Color.FromArgb(0xFF, 0xFF, 0xB3, 0x00),           // amber
            "acronyms" => Color.FromArgb(0xFF, 0xFF, 0x70, 0x43),        // deep orange
            "imbruttito" => Color.FromArgb(0xFF, 0xD8, 0x43, 0x15),      // milano red
            "custom" => Color.FromArgb(0xFF, 0x5C, 0x6B, 0xC0),          // indigo lighter
            _ => Color.FromArgb(0xFF, 0x9E, 0x9E, 0x9E),
        };
        return new SolidColorBrush(c);
    }

    public object ConvertBack(object value, Type targetType, object parameter, string language) =>
        Microsoft.UI.Xaml.DependencyProperty.UnsetValue;
}

using System;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Data;

namespace Dimmy.Windows.Converters;

/// Visible when the bound value is non-null AND, for strings, non-empty.
/// Collapsed otherwise. Used in the History detail panel so the Enhanced
/// section auto-hides when no enhanced text is present.
public sealed class NotNullToVisibilityConverter : IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, string language)
    {
        bool invert = parameter is string p && p == "Invert";
        bool present = value switch
        {
            null => false,
            string s => !string.IsNullOrEmpty(s),
            _ => true,
        };
        if (invert) present = !present;
        return present ? Visibility.Visible : Visibility.Collapsed;
    }

    public object ConvertBack(object value, Type targetType, object parameter, string language)
        => throw new NotSupportedException();
}

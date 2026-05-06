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
        if (value is null) return Visibility.Collapsed;
        if (value is string s && string.IsNullOrEmpty(s)) return Visibility.Collapsed;
        return Visibility.Visible;
    }

    public object ConvertBack(object value, Type targetType, object parameter, string language)
        => throw new NotSupportedException();
}

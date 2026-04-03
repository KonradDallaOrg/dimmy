using System;
using Microsoft.UI.Xaml.Data;

namespace Dimmy.Windows.Converters;

public sealed class StringEqualityConverter : IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, string language)
    {
        return value is string s && parameter is string p
            && string.Equals(s, p, StringComparison.OrdinalIgnoreCase);
    }

    public object ConvertBack(object value, Type targetType, object parameter, string language)
    {
        if (value is bool b && b && parameter is string p) return p;
        return Microsoft.UI.Xaml.DependencyProperty.UnsetValue;
    }
}

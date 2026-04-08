using System;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Data;

namespace Dimmy.Windows.Converters;

public sealed class BoolToVisibilityConverter : IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, string language)
    {
        bool invert = parameter is string s && s == "Invert";
        bool visible = value is bool b && b;
        if (invert) visible = !visible;
        return visible ? Visibility.Visible : Visibility.Collapsed;
    }

    public object ConvertBack(object value, Type targetType, object parameter, string language)
    {
        bool invert = parameter is string s && s == "Invert";
        bool visible = value is Visibility v && v == Visibility.Visible;
        return invert ? !visible : visible;
    }
}

using System;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Data;
using Microsoft.UI.Xaml.Media;

namespace Dimmy.Windows.Converters;

public sealed class BoolToAccentBrushConverter : IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, string language)
    {
        bool isTrue = value is bool b && b;
        var key = isTrue ? "AccentFillColorDefaultBrush" : "CardStrokeColorDefaultBrush";
        return (Brush)Application.Current.Resources[key];
    }

    public object ConvertBack(object value, Type targetType, object parameter, string language) =>
        throw new NotImplementedException();
}

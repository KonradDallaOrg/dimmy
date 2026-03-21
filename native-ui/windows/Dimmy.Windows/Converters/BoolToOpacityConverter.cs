using Microsoft.UI.Xaml.Data;

namespace Dimmy.Windows.Converters;

public sealed class BoolToOpacityConverter : IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, string language)
    {
        bool reached = value is bool b && b;
        bool invert = parameter is string s && s == "Invert";
        if (invert) reached = !reached;
        return reached ? 1.0 : 0.3;
    }

    public object ConvertBack(object value, Type targetType, object parameter, string language)
        => throw new NotImplementedException();
}

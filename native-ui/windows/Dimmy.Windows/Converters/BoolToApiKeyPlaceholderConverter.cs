using Microsoft.UI.Xaml.Data;

namespace Dimmy.Windows.Converters;

/// <summary>
/// Converts a boolean HasApiKey to a placeholder string for a PasswordBox.
/// true  → "Key saved — enter new to replace"
/// false → "Enter API key"
/// </summary>
public sealed class BoolToApiKeyPlaceholderConverter : IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, string language)
    {
        return value is bool b && b
            ? "Key saved — enter new to replace"
            : "Enter API key";
    }

    public object ConvertBack(object value, Type targetType, object parameter, string language)
        => throw new NotSupportedException();
}

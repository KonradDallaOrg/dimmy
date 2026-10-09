using System.Text.RegularExpressions;

namespace Dimmy.Windows.Helpers;

/// <summary>Whether what the user typed can be an email address. A shape
/// check that keeps a typo from reaching the server; the magic link is what
/// proves the address exists. Mirror of Mac <c>EmailInput.looksValid</c>.</summary>
public static class EmailInput
{
    private static readonly Regex Shape =
        new(@"^[^@\s]+@[^@\s.]+(\.[^@\s.]+)+$", RegexOptions.Compiled);

    public static bool LooksValid(string? text)
    {
        var trimmed = (text ?? string.Empty).Trim();
        return trimmed.Length <= 254 && Shape.IsMatch(trimmed);
    }
}

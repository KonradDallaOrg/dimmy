using System;
using System.IO;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;

namespace Dimmy.Windows.Helpers;

/// <summary>The vendor's mark from Assets/Providers. SVG for most vendors;
/// PNG for a vendor whose official mark exists only as a bitmap (Kimi).
/// Brand marks are used exactly as the vendor publishes them.</summary>
internal static class ProviderIcon
{
    public static ImageSource Source(string id)
    {
        var png = Path.Combine(AppContext.BaseDirectory, "Assets", "Providers", id + ".png");
        if (File.Exists(png))
            return new BitmapImage(new Uri($"ms-appx:///Assets/Providers/{id}.png"));
        return new SvgImageSource(new Uri($"ms-appx:///Assets/Providers/{id}.svg"));
    }
}

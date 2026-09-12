namespace Dimmy.Windows.Services;

/// <summary>
/// Which version string the UI shows, and where it is allowed to come from.
///
/// <para>There are two answers available and they are not equivalent:</para>
/// <list type="bullet">
/// <item><b>Velopack</b> knows what this install IS, suffix included:
/// "0.7.3-rc.1". It is the only component that does.</item>
/// <item><b>The Rust core</b> returns CARGO_PKG_VERSION, "0.7.3". The
/// pre-release suffix lives in the git tag and the package, and is
/// deliberately not baked into the Rust build, because that would miss the
/// prebuilt-DLL cache on every single tag.</item>
/// </list>
///
/// <para>So the core's answer is not a worse version of the truth, it is a
/// DIFFERENT claim: "this is some build of 0.7.3". Showing it to someone
/// running a release candidate tells them they are on the stable release.
/// That has now been reported twice on a live rc, the second time with the
/// About page reading "Dimmy 0.7.3" directly above a banner reading
/// "v0.7.3-rc.1" — the same window contradicting itself.</para>
/// </summary>
public static class VersionDisplay
{
    /// <summary>Pick the version to show.
    ///
    /// <para><paramref name="packaged"/> is empty only when there is no
    /// Velopack identity at all, which means an unpackaged dev build — the
    /// one case where the core's suffix-less answer is actually true.</para>
    /// </summary>
    public static string Resolve(string? packaged, string? core)
    {
        if (!string.IsNullOrWhiteSpace(packaged)) return packaged!.Trim();
        if (!string.IsNullOrWhiteSpace(core)) return core!.Trim();
        return "0.0.0";
    }

    /// <summary>True when the two sources disagree by a pre-release suffix,
    /// i.e. taking the core's answer would silently drop it.
    ///
    /// <para>Not used to decide anything — <see cref="Resolve"/> already
    /// prefers the packaged string. It exists so the regression can be
    /// stated as a test instead of as a comment.</para>
    /// </summary>
    public static bool WouldLoseSuffix(string? packaged, string? core)
    {
        if (string.IsNullOrWhiteSpace(packaged) || string.IsNullOrWhiteSpace(core))
            return false;
        int dash = packaged!.IndexOf('-');
        if (dash < 0) return false;
        return string.Equals(packaged[..dash], core!.Trim(),
            System.StringComparison.OrdinalIgnoreCase);
    }
}

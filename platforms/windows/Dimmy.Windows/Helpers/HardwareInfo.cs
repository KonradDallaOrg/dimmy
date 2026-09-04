using System;
using System.Text.Json;

namespace Dimmy.Windows.Helpers;

/// <summary>What the Rust core could learn about the graphics device.
/// Mirror of `core/src/hardware.rs::GpuInfo` + its verdict.
///
/// Read by two surfaces: the onboarding Local card (which promised
/// "runs on your machine" while knowing nothing about the machine) and
/// the GPU ACCELERATION diagnostics card (which said Enabled/Disabled
/// without naming the device).
///
/// `Fitness` answers only whether the shipped models FIT, never how fast
/// they will be — the same 4 GB card ran whisper at 8 s and at 2 s per
/// window on one day, the difference being a stuck power limit. `Line` is
/// null when there is nothing honest to say and the caller must then
/// render nothing rather than a placeholder.</summary>
public sealed record HardwareInfo(
    string? Name,
    long? VramMb,
    bool Dedicated,
    bool AppleSilicon,
    string Fitness,
    string? Line)
{
    /// <summary>Parse what <c>dimmy_hardware_json</c> returned. Any
    /// malformed or missing payload yields <c>null</c>: a hardware hint is
    /// never worth breaking a page over, and the surfaces are written to
    /// show nothing when there is nothing.</summary>
    public static HardwareInfo? Parse(string? json)
    {
        if (string.IsNullOrWhiteSpace(json)) return null;
        try
        {
            using var doc = JsonDocument.Parse(json);
            var root = doc.RootElement;
            if (root.ValueKind != JsonValueKind.Object) return null;
            return new HardwareInfo(
                Name: Str(root, "name"),
                VramMb: Num(root, "vram_mb"),
                Dedicated: Bool(root, "dedicated"),
                AppleSilicon: Bool(root, "apple_silicon"),
                Fitness: Str(root, "fitness") ?? "unknown",
                Line: Str(root, "line"));
        }
        catch (JsonException)
        {
            return null;
        }
    }

    private static string? Str(JsonElement o, string key) =>
        o.TryGetProperty(key, out var v) && v.ValueKind == JsonValueKind.String
            ? v.GetString()
            : null;

    private static long? Num(JsonElement o, string key) =>
        o.TryGetProperty(key, out var v) && v.ValueKind == JsonValueKind.Number
            && v.TryGetInt64(out var n)
            ? n
            : null;

    private static bool Bool(JsonElement o, string key) =>
        o.TryGetProperty(key, out var v) && v.ValueKind == JsonValueKind.True;
}

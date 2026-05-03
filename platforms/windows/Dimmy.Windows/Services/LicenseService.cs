using System;
using System.Collections.Generic;
using System.Linq;
using System.Text;
using System.Text.Json;
using System.Text.Json.Serialization;
using System.Threading.Tasks;
using Dimmy.Windows.Interop;

namespace Dimmy.Windows.Services;

/// <summary>
/// Strongly-typed wrapper over the licensing FFI. All methods marshal to
/// the Rust core (which speaks to the licensing server). UI binds to the
/// returned status records / response objects directly.
/// </summary>
public static class LicenseService
{
    private const int Buf = 4096;

    public sealed record Status(
        string Kind,
        string? Tier,
        long? DaysRemaining,
        int? DaysOffline,
        string? Error,
        bool CloudEnabled,
        bool UpdatesEnabled,
        IReadOnlyList<string> Scopes,
        long? CancelsAt);

    public static class ScopeNames
    {
        public const string ManagedStt    = "managed_stt";
        public const string ManagedLlm    = "managed_llm";
        public const string AutoUpdate    = "auto_update";
        public const string HistorySync   = "history_sync";
        public const string PremiumStyles = "premium_styles";

        // Driven by the order shown in the Settings → License scope grid.
        public static readonly (string Key, string Display, string Description)[] All = new[]
        {
            (ManagedStt,    "Managed STT",     "Use Dimmy-hosted speech-to-text without configuring your own provider key."),
            (ManagedLlm,    "Managed LLM",     "Use Dimmy-hosted post-processing LLM without configuring your own key."),
            (AutoUpdate,    "Auto-update",     "Receive in-app version updates as soon as they ship."),
            (HistorySync,   "History sync",    "Sync your transcription history across devices (planned)."),
            (PremiumStyles, "Premium styles",  "Curated LLM styles and prompts beyond the defaults (planned)."),
        };
    }

    public sealed record OpResult(bool Ok, string? Error, string? MagicLink, string? Code);

    public sealed record DeviceInfo(
        string DeviceId,
        string Label,
        long IssuedAt,
        long LastSeen,
        string Status,
        bool IsSelf);

    public sealed record DevicesList(
        bool Ok,
        string? Error,
        string? LicenseId,
        string? Tier,
        int MaxDevices,
        IReadOnlyList<DeviceInfo> Devices);

    /// Fired when a license-state-changing operation completes (redeem, refresh,
    /// clear). Settings → License subscribes so it can refresh status without
    /// polling. Fires on whatever thread did the change — UI must marshal.
    public static event Action? LicenseChanged;
    public static void NotifyChanged() => LicenseChanged?.Invoke();

    public static Status GetStatus()
    {
        var buf = new byte[Buf];
        int n = DimmyNative.dimmy_license_status_json(buf, buf.Length);
        if (n < 0)
            return new Status("Invalid", null, null, null, "FFI returned " + n, false, false, Array.Empty<string>(), null);
        var json = Encoding.UTF8.GetString(buf, 0, n);
        var w = JsonSerializer.Deserialize<StatusWire>(json, JsonOpts);
        if (w is null)
            return new Status("Invalid", null, null, null, "deserialize", false, false, Array.Empty<string>(), null);
        return new Status(
            w.Kind ?? "Invalid",
            w.Tier,
            w.DaysRemaining,
            w.DaysOffline,
            w.Error,
            w.CloudEnabled,
            w.UpdatesEnabled,
            (IReadOnlyList<string>?)w.Scopes ?? Array.Empty<string>(),
            w.CancelsAt);
    }

    /// True if the active license carries the named scope. Source builds
    /// (no embedded pubkey) return true for everything.
    public static bool HasScope(string scopeName)
    {
        if (string.IsNullOrEmpty(scopeName)) return false;
        return DimmyNative.dimmy_license_has_scope(scopeName) == 1;
    }

    public static Task<OpResult> RequestTrialAsync(string email) =>
        Task.Run(() =>
        {
            var buf = new byte[Buf];
            int n = DimmyNative.dimmy_license_request_trial(email ?? string.Empty, buf, buf.Length);
            return ParseOp(buf, n);
        });

    public static Task<OpResult> RedeemAsync(string code, string deviceLabel) =>
        Task.Run(() =>
        {
            var buf = new byte[Buf];
            int n = DimmyNative.dimmy_license_redeem(
                code ?? string.Empty,
                string.IsNullOrWhiteSpace(deviceLabel) ? "Unknown device" : deviceLabel,
                buf, buf.Length);
            return ParseOp(buf, n);
        });

    public static Task<OpResult> RefreshAsync() =>
        Task.Run(() =>
        {
            var buf = new byte[Buf];
            int n = DimmyNative.dimmy_license_refresh(buf, buf.Length);
            return ParseOp(buf, n);
        });

    public static int Clear() => DimmyNative.dimmy_license_clear();

    public static Task<DevicesList> ListDevicesAsync() =>
        Task.Run(() =>
        {
            var buf = new byte[8192];
            int n = DimmyNative.dimmy_license_devices_list(buf, buf.Length);
            if (n < 0)
                return new DevicesList(false, "FFI returned " + n, null, null, 0, Array.Empty<DeviceInfo>());
            var json = Encoding.UTF8.GetString(buf, 0, n);
            var w = JsonSerializer.Deserialize<DevicesListWire>(json, JsonOpts);
            if (w is null)
                return new DevicesList(false, "deserialize", null, null, 0, Array.Empty<DeviceInfo>());
            var devices = (w.Devices ?? new List<DeviceWire>())
                .Select(d => new DeviceInfo(
                    d.DeviceId ?? string.Empty,
                    d.Label ?? string.Empty,
                    d.IssuedAt, d.LastSeen,
                    d.Status ?? "unknown",
                    d.IsSelf))
                .ToList();
            return new DevicesList(w.Ok, w.Error, w.LicenseId, w.Tier, w.MaxDevices, devices);
        });

    public static Task<OpResult> DeactivateDeviceAsync(string? deviceId) =>
        Task.Run(() =>
        {
            var buf = new byte[1024];
            int n = DimmyNative.dimmy_license_device_deactivate(deviceId, buf, buf.Length);
            return ParseOp(buf, n);
        });

    public sealed record UrlResult(bool Ok, string? Url, string? Error);

    /// POST /api/checkout/create — returns Stripe Checkout URL for the chosen tier.
    /// Caller opens the URL in the system browser; webhook handles the rest.
    public static Task<UrlResult> CreateCheckoutAsync(string tier) =>
        Task.Run(() =>
        {
            var buf = new byte[2048];
            int n = DimmyNative.dimmy_license_checkout_url(tier, buf, buf.Length);
            return ParseUrl(buf, n);
        });

    /// POST /api/billing-portal — returns Stripe Customer Portal URL.
    /// Only valid for licenses with stripe_customer_id (paid).
    public static Task<UrlResult> BillingPortalUrlAsync() =>
        Task.Run(() =>
        {
            var buf = new byte[2048];
            int n = DimmyNative.dimmy_license_billing_portal_url(buf, buf.Length);
            return ParseUrl(buf, n);
        });

    private static UrlResult ParseUrl(byte[] buf, int n)
    {
        if (n < 0) return new UrlResult(false, null, "FFI returned " + n);
        var json = Encoding.UTF8.GetString(buf, 0, n);
        var w = JsonSerializer.Deserialize<UrlWire>(json, JsonOpts);
        if (w is null) return new UrlResult(false, null, "deserialize");
        return new UrlResult(w.Ok, w.Url, w.Error);
    }

    private sealed class UrlWire
    {
        [JsonPropertyName("ok")]    public bool Ok { get; set; }
        [JsonPropertyName("url")]   public string? Url { get; set; }
        [JsonPropertyName("error")] public string? Error { get; set; }
    }

    private static OpResult ParseOp(byte[] buf, int n)
    {
        if (n < 0) return new OpResult(false, "FFI returned " + n, null, null);
        var json = Encoding.UTF8.GetString(buf, 0, n);
        var w = JsonSerializer.Deserialize<OpWire>(json, JsonOpts);
        if (w is null) return new OpResult(false, "deserialize", null, null);
        return new OpResult(w.Ok, w.Error, w.MagicLink, w.Code);
    }

    private static readonly JsonSerializerOptions JsonOpts = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.SnakeCaseLower,
        DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
    };

    private sealed class StatusWire
    {
        [JsonPropertyName("kind")]           public string? Kind { get; set; }
        [JsonPropertyName("tier")]           public string? Tier { get; set; }
        [JsonPropertyName("days_remaining")] public long? DaysRemaining { get; set; }
        [JsonPropertyName("days_offline")]   public int? DaysOffline { get; set; }
        [JsonPropertyName("error")]          public string? Error { get; set; }
        [JsonPropertyName("cloud_enabled")]  public bool CloudEnabled { get; set; }
        [JsonPropertyName("updates_enabled")] public bool UpdatesEnabled { get; set; }
        [JsonPropertyName("scopes")]         public List<string>? Scopes { get; set; }
        [JsonPropertyName("cancels_at")]     public long? CancelsAt { get; set; }
    }

    private sealed class OpWire
    {
        [JsonPropertyName("ok")]         public bool Ok { get; set; }
        [JsonPropertyName("error")]      public string? Error { get; set; }
        [JsonPropertyName("magic_link")] public string? MagicLink { get; set; }
        [JsonPropertyName("code")]       public string? Code { get; set; }
    }

    private sealed class DevicesListWire
    {
        [JsonPropertyName("ok")]          public bool Ok { get; set; }
        [JsonPropertyName("error")]       public string? Error { get; set; }
        [JsonPropertyName("license_id")]  public string? LicenseId { get; set; }
        [JsonPropertyName("tier")]        public string? Tier { get; set; }
        [JsonPropertyName("max_devices")] public int MaxDevices { get; set; }
        [JsonPropertyName("devices")]     public List<DeviceWire>? Devices { get; set; }
    }

    private sealed class DeviceWire
    {
        [JsonPropertyName("device_id")] public string? DeviceId { get; set; }
        [JsonPropertyName("label")]     public string? Label { get; set; }
        [JsonPropertyName("issued_at")] public long IssuedAt { get; set; }
        [JsonPropertyName("last_seen")] public long LastSeen { get; set; }
        [JsonPropertyName("status")]    public string? Status { get; set; }
        [JsonPropertyName("is_self")]   public bool IsSelf { get; set; }
    }
}

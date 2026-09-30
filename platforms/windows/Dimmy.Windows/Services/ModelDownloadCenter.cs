using System;
using System.Collections.Generic;
using System.Text.Json;

namespace Dimmy.Windows.Services;

/// <summary>One on-device model download as the core reports it
/// (<c>model_download</c> event / snapshot entry). State is one of
/// queued · downloading · done · failed · cancelled.</summary>
public sealed record ModelDownloadJob(string Id, string State, long Done, long Total, string? Error)
{
    public bool IsActive => State is "queued" or "downloading";

    /// <summary>Null while the size is unknown: show an indeterminate ring.</summary>
    public double? Percent => Total > 0 ? Math.Min(100.0, Done * 100.0 / Total) : null;
}

/// <summary>Host mirror of core/src/download_center.rs. The queue lives in the
/// core, so a download keeps going (and keeps its %) whatever page is open;
/// this only remembers the last state per model and tells whoever is showing
/// one. Fed by <c>AppViewModel.HandleEvent</c> on the UI thread — no locks.</summary>
public sealed class ModelDownloadCenter
{
    public static ModelDownloadCenter Instance { get; } = new();

    private readonly Dictionary<string, ModelDownloadJob> _jobs = new(StringComparer.Ordinal);

    public event Action<ModelDownloadJob>? Changed;

    public ModelDownloadJob? Get(string id) => _jobs.TryGetValue(id, out var j) ? j : null;

    public void Apply(string payloadJson)
    {
        try
        {
            using var doc = JsonDocument.Parse(payloadJson);
            Store(doc.RootElement);
        }
        catch (JsonException) { }
    }

    public void LoadSnapshot(string? json)
    {
        if (string.IsNullOrEmpty(json)) return;
        try
        {
            using var doc = JsonDocument.Parse(json);
            if (doc.RootElement.ValueKind != JsonValueKind.Array) return;
            foreach (var el in doc.RootElement.EnumerateArray()) Store(el);
        }
        catch (JsonException) { }
    }

    private void Store(JsonElement el)
    {
        if (el.ValueKind != JsonValueKind.Object) return;
        if (!el.TryGetProperty("id", out var idEl) || idEl.GetString() is not { Length: > 0 } id) return;
        if (!el.TryGetProperty("state", out var stEl) || stEl.GetString() is not { Length: > 0 } state) return;
        long done = el.TryGetProperty("done", out var d) && d.TryGetInt64(out var dv) ? dv : 0;
        long total = el.TryGetProperty("total", out var t) && t.TryGetInt64(out var tv) ? tv : 0;
        string? error = el.TryGetProperty("error", out var e) ? e.GetString() : null;

        var job = new ModelDownloadJob(id, state, done, total, error);
        _jobs[id] = job;
        Changed?.Invoke(job);
    }
}

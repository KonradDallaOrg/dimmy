using System;
using System.Collections.Generic;
using System.Linq;
using System.Text.Json;
using System.Text.Json.Serialization;
using Dimmy.Windows.Interop;

namespace Dimmy.Windows.Services;

// Single source of truth for the cloud models/providers Dimmy offers.
// The data is the JSON embedded in the Rust core (assets/model-catalog.json),
// read once at startup via the dimmy_model_catalog_json() FFI. Every model
// picker on this platform — and the macOS app, reading the same bytes — is
// built from this, so the lists can no longer drift apart by hand.
//
// Local models (Whisper / Parakeet / Gemma / Phi) are NOT here: they have
// download + bundle + feature-flag semantics and are injected separately at
// picker-build time.

public sealed class CatalogModel
{
    [JsonPropertyName("id")] public string Id { get; set; } = "";
    [JsonPropertyName("label")] public string Label { get; set; } = "";
    [JsonPropertyName("tasks")] public List<string> Tasks { get; set; } = new();
    [JsonPropertyName("tier")] public string? Tier { get; set; }
}

public sealed class CatalogProvider
{
    [JsonPropertyName("id")] public string Id { get; set; } = "";
    [JsonPropertyName("name")] public string Name { get; set; } = "";
    [JsonPropertyName("icon")] public string Icon { get; set; } = "";
    [JsonPropertyName("llm_url")] public string LlmUrl { get; set; } = "";
    [JsonPropertyName("stt_url")] public string SttUrl { get; set; } = "";
    [JsonPropertyName("models")] public List<CatalogModel> Models { get; set; } = new();
}

internal sealed class CatalogRoot
{
    [JsonPropertyName("schema")] public int Schema { get; set; }
    [JsonPropertyName("providers")] public List<CatalogProvider> Providers { get; set; } = new();
}

/// <summary>One concrete model choice for the STT or LLM provider picker:
/// the synthetic preset Name (used as the ComboBox item Tag), the endpoint a
/// selection resolves to, the exact model id sent to the API, and the display
/// chrome (icon + label).</summary>
public sealed record PickerEntry(string Name, string Url, string Model, string Icon, string Label);

/// <summary>One recap-model choice: the model id (Tag), display label,
/// provider icon, and provider id (vendor) the recap dispatch derives from.</summary>
public sealed record RecapEntry(string Id, string Label, string Icon, string ProviderId);

public static class ModelCatalog
{
    public static IReadOnlyList<CatalogProvider> Providers { get; }

    static ModelCatalog()
    {
        CatalogRoot? root = null;
        try
        {
            // The FFI call must be inside the try too: in a unit-test host (or
            // any process where dimmy_lib isn't on the load path) the P/Invoke
            // throws DllNotFoundException, and since this runs from
            // SettingsViewModel's static initializer it would otherwise take
            // down every test that merely touches the view-model. Degrade to
            // an empty catalog instead — the picker shows only its Custom
            // fallback, which is visible rather than a crash. In the real app
            // the DLL is always loaded, so this path is test/edge only.
            var json = DimmyNative.ModelCatalogJson();
            if (!string.IsNullOrEmpty(json))
            {
                root = JsonSerializer.Deserialize<CatalogRoot>(json);
            }
        }
        catch
        {
            root = null;
        }
        Providers = root?.Providers ?? new List<CatalogProvider>();
    }

    static IEnumerable<(CatalogProvider P, CatalogModel M)> ModelsFor(string task) =>
        Providers.SelectMany(p => p.Models
            .Where(m => m.Tasks.Contains(task))
            .Select(m => (p, m)));

    // The synthetic preset Name doubles as the ComboBox item Tag and the key
    // SelectionChanged matches on. It must be unique and lowercase (handlers
    // lowercase the tag before comparing). provider-id + model-id is unique
    // because model ids are unique within a provider. The exact-case model id
    // travels separately in PickerEntry.Model.
    static string PresetName(CatalogProvider p, CatalogModel m) =>
        $"{p.Id}-{m.Id}".ToLowerInvariant();

    static string Label(CatalogProvider p, CatalogModel m) => $"{p.Name} · {m.Label}";

    /// <summary>Cloud STT choices, in catalog order. Consumers append their
    /// own "Custom" entry.</summary>
    public static IReadOnlyList<PickerEntry> SttEntries() =>
        ModelsFor("stt")
            .Select(x => new PickerEntry(PresetName(x.P, x.M), x.P.SttUrl, x.M.Id, x.P.Icon, Label(x.P, x.M)))
            .ToList();

    /// <summary>Cloud LLM (dictation rewrite) choices, in catalog order.</summary>
    public static IReadOnlyList<PickerEntry> LlmEntries() =>
        ModelsFor("llm")
            .Select(x => new PickerEntry(PresetName(x.P, x.M), x.P.LlmUrl, x.M.Id, x.P.Icon, Label(x.P, x.M)))
            .ToList();

    /// <summary>Cloud recap choices, in catalog order. The Tag is the bare
    /// model id (recap dispatch derives the vendor from it).</summary>
    public static IReadOnlyList<RecapEntry> RecapEntries() =>
        ModelsFor("recap")
            .Select(x => new RecapEntry(x.M.Id, x.M.Label, x.P.Icon, x.P.Id))
            .ToList();

    /// <summary>All curated recap model ids (the empty "Auto" tag plus every
    /// cloud recap model). Used to tell a curated pick from a custom one.</summary>
    public static IReadOnlyList<string> RecapKnownTags()
    {
        var tags = new List<string> { "" };
        tags.AddRange(RecapEntries().Select(e => e.Id));
        return tags;
    }

    public static CatalogProvider? ById(string id) =>
        Providers.FirstOrDefault(p => string.Equals(p.Id, id, StringComparison.OrdinalIgnoreCase));
}

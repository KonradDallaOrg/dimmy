using System;
using System.Linq;
using Dimmy.Windows.Services;
using Xunit;

namespace Dimmy.Windows.Tests;

/// <summary>
/// Guards the data that drives the new Providers &amp; Keys page. The catalog is
/// the single place the UI learns which capabilities (STT / LLM / Recap) each
/// provider serves and where to send the user to fetch a key. If this data
/// drifts from core/src/provider.rs the UI silently offers the wrong thing
/// (e.g. a "save STT key" affordance for Anthropic, which has no STT endpoint),
/// so the capability truth is pinned here.
/// </summary>
public class ProviderCatalogTests
{
    [Fact]
    public void All_providers_have_identity_fields()
    {
        Assert.NotEmpty(ProviderCatalog.All);
        foreach (var p in ProviderCatalog.All)
        {
            Assert.False(string.IsNullOrWhiteSpace(p.Id), $"{p.Name} has empty Id");
            Assert.False(string.IsNullOrWhiteSpace(p.Name), $"{p.Id} has empty Name");
            Assert.False(string.IsNullOrWhiteSpace(p.Mark), $"{p.Id} has empty Mark");
            Assert.Equal(2, p.Mark.Length);
            Assert.False(string.IsNullOrWhiteSpace(p.AccentHex), $"{p.Id} has empty AccentHex");
            Assert.StartsWith("#", p.AccentHex);
        }
    }

    [Fact]
    public void Provider_ids_are_unique()
    {
        var ids = ProviderCatalog.All.Select(p => p.Id.ToLowerInvariant()).ToList();
        Assert.Equal(ids.Count, ids.Distinct().Count());
    }

    // Capability truth — mirrors core/src/provider.rs.
    [Theory]
    [InlineData("local", true, true)]
    [InlineData("groq", true, true)]
    [InlineData("openai", true, true)]
    [InlineData("gemini", true, true)]
    [InlineData("fireworks", true, true)]
    [InlineData("deepgram", true, false)]   // STT only
    [InlineData("anthropic", false, true)]  // LLM/Recap only
    [InlineData("openrouter", false, true)] // LLM/Recap only
    [InlineData("together", true, true)]    // Together offers STT (parakeet/whisper) + LLM in the Win picker
    [InlineData("custom", true, true)]
    public void Capability_truth_matches_core(string id, bool stt, bool llm)
    {
        var p = ProviderCatalog.ById(id);
        Assert.NotNull(p);
        Assert.Equal(stt, p!.Stt);
        Assert.Equal(llm, p.Llm);
    }

    [Fact]
    public void Recap_capability_equals_llm()
    {
        // Recap is an LLM operation — any LLM-capable provider can recap, and
        // no non-LLM provider can. The extension method must never diverge.
        foreach (var p in ProviderCatalog.All)
            Assert.Equal(p.Llm, p.Recap());
    }

    [Fact]
    public void Every_provider_serves_at_least_one_capability()
    {
        foreach (var p in ProviderCatalog.All)
            Assert.True(p.Stt || p.Llm, $"{p.Id} serves no capability");
    }

    [Fact]
    public void Keyed_providers_have_an_https_console_deeplink()
    {
        // The "get a key" deep-link is the bombproof part of the wizard. Every
        // provider that needs a key (i.e. not on-device, not custom-endpoint)
        // must point at a real HTTPS console page.
        foreach (var p in ProviderCatalog.All)
        {
            bool needsKey = p.Id is not ("local" or "custom");
            if (!needsKey)
            {
                Assert.Equal(string.Empty, p.ConsoleUrl);
                continue;
            }
            Assert.False(string.IsNullOrWhiteSpace(p.ConsoleUrl), $"{p.Id} has no console URL");
            Assert.StartsWith("https://", p.ConsoleUrl);
            // Must be a parseable absolute URI so HyperlinkButton.NavigateUri
            // never throws at runtime.
            Assert.True(Uri.TryCreate(p.ConsoleUrl, UriKind.Absolute, out _),
                $"{p.id_or_url()} is not an absolute URI");
        }
    }

    [Fact]
    public void Get_key_hint_is_present_for_every_provider()
    {
        foreach (var p in ProviderCatalog.All)
            Assert.False(string.IsNullOrWhiteSpace(p.GetKeyHint), $"{p.Id} has no GetKeyHint");
    }

    [Fact]
    public void Model_capabilities_never_exceed_provider_capabilities()
    {
        // A model row can't advertise a capability the provider can't serve —
        // otherwise the badges lie. (A provider CAN have models that each cover
        // a subset; the union just must not exceed the provider flags.)
        foreach (var p in ProviderCatalog.All)
        {
            Assert.NotEmpty(p.Models);
            foreach (var m in p.Models)
            {
                if (m.Stt) Assert.True(p.Stt, $"{p.Id}/{m.Name} claims STT but provider can't");
                if (m.Llm) Assert.True(p.Llm, $"{p.Id}/{m.Name} claims LLM but provider can't");
                if (m.Recap) Assert.True(p.Llm, $"{p.Id}/{m.Name} claims Recap but provider has no LLM");
            }
        }
    }

    [Fact]
    public void ById_is_case_insensitive_and_returns_null_for_unknown()
    {
        Assert.NotNull(ProviderCatalog.ById("GROQ"));
        Assert.NotNull(ProviderCatalog.ById("Groq"));
        Assert.Null(ProviderCatalog.ById("does-not-exist"));
    }

    [Fact]
    public void On_device_provider_needs_no_key()
    {
        var local = ProviderCatalog.ById("local");
        Assert.NotNull(local);
        Assert.Equal(string.Empty, local!.ConsoleUrl);
    }
}

internal static class ProviderInfoTestExt
{
    // Tiny helper so the assertion message can name the offending provider.
    public static string id_or_url(this ProviderInfo p) => $"{p.Id} ({p.ConsoleUrl})";
}

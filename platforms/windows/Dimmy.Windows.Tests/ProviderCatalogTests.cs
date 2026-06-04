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
            foreach (var m in p.Models)
            {
                if (m.Stt) Assert.True(p.Stt, $"{p.Id}/{m.Name} claims STT but provider can't");
                if (m.Llm) Assert.True(p.Llm, $"{p.Id}/{m.Name} claims LLM but provider can't");
                if (m.Recap) Assert.True(p.Llm, $"{p.Id}/{m.Name} claims Recap but provider has no LLM");
            }
        }
        // local + custom rows are code-defined (not catalog-derived), so they
        // are always populated. Cloud providers' model rows come from the
        // embedded catalog (empty in a DLL-less unit-test host); their content
        // is verified by the Rust catalog::tests, so don't require it here.
        Assert.NotEmpty(ProviderCatalog.ById("local")!.Models);
        Assert.NotEmpty(ProviderCatalog.ById("custom")!.Models);
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

    // ── Key save scopes (what the Providers page writes per provider) ──
    // dimmy_save_llm_provider_key now accepts "stt" for speech vendors (incl.
    // Deepgram) and "llm"+"recap" for completions vendors. Custom needs a URL so
    // it's keyed on Voice/Output; On-device needs no key.

    [Theory]
    [InlineData("groq", true)]
    [InlineData("openai", true)]
    [InlineData("anthropic", true)]
    [InlineData("gemini", true)]
    [InlineData("openrouter", true)]
    [InlineData("fireworks", true)]
    [InlineData("together", true)]
    [InlineData("deepgram", true)]  // STT-only, now keyable via the "stt" scope
    [InlineData("custom", false)]   // needs a base URL — keyed on Voice/Output
    [InlineData("local", false)]    // on-device — no key at all
    public void IsKeyableHere_matches_ffi_acceptance(string id, bool keyable)
    {
        var p = ProviderCatalog.ById(id);
        Assert.NotNull(p);
        Assert.Equal(keyable, p!.IsKeyableHere());
    }

    // Exact scope set per provider, mirroring Provider::supports_stt +
    // default_llm_url in core/src/provider.rs.
    [Theory]
    [InlineData("groq", "stt,llm,recap")]
    [InlineData("openai", "stt,llm,recap")]
    [InlineData("gemini", "stt,llm,recap")]
    [InlineData("fireworks", "stt,llm,recap")]
    [InlineData("together", "stt,llm,recap")]
    [InlineData("anthropic", "llm,recap")]   // LLM-only
    [InlineData("openrouter", "llm,recap")]  // LLM-only
    [InlineData("deepgram", "stt")]          // STT-only
    [InlineData("custom", "")]
    [InlineData("local", "")]
    public void KeySaveScopes_match_provider_capabilities(string id, string expected)
    {
        var p = ProviderCatalog.ById(id);
        Assert.NotNull(p);
        var want = expected.Length == 0 ? System.Array.Empty<string>() : expected.Split(',');
        Assert.Equal(want, p!.KeySaveScopes());
    }

    [Fact]
    public void Key_scopes_are_a_subset_of_what_the_provider_serves()
    {
        // Never write a key into a scope the provider can't serve.
        foreach (var p in ProviderCatalog.All)
            foreach (var scope in p.KeySaveScopes())
            {
                if (scope == "stt") Assert.True(p.Stt, $"{p.Id} writes stt but has no STT capability");
                if (scope is "llm" or "recap") Assert.True(p.Llm, $"{p.Id} writes {scope} but has no LLM capability");
            }
    }

    [Fact]
    public void Stt_capable_keyable_providers_write_the_stt_scope()
    {
        // The whole point of the FFI fix: an STT-capable provider keyed from the
        // Providers page now populates the STT scope too (one key per provider).
        foreach (var p in ProviderCatalog.All.Where(p => p.IsKeyableHere() && p.Stt))
            Assert.Contains("stt", p.KeySaveScopes());
    }
}

internal static class ProviderInfoTestExt
{
    // Tiny helper so the assertion message can name the offending provider.
    public static string id_or_url(this ProviderInfo p) => $"{p.Id} ({p.ConsoleUrl})";
}

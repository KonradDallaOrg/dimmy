using System;
using System.Linq;
using Dimmy.Windows.Services;
using Microsoft.UI.Text;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;
using Microsoft.UI.Xaml.Shapes;
using WinColor = global::Windows.UI.Color;

namespace Dimmy.Windows.Views;

/// <summary>
/// Providers &amp; keys page — built programmatically (no XAML DataTemplates) so
/// there is zero XAML-compiler surface. One card per provider from
/// <see cref="ProviderCatalog"/>: real brand logo, status, key entry, a
/// deep-link "Get key →" wizard to the exact console, and a per-model
/// capability list.
///
/// Visual rules (locked decisions):
///   • Native Fluent, theme-aware — every colour comes from a ThemeResource
///     brush, NOT a hardcoded hex, so the page looks right in light AND dark.
///     The ONE exception is the logo tile, which is intentionally a white
///     squircle (favicon-tile convention) so monochrome currentColor marks
///     (groq/openai/anthropic/openrouter) stay visible in both themes.
///   • Backend UNCHANGED: keys go through dimmy_save_llm_provider_key into the
///     existing AES-256 keystore (scopes stt/llm/recap per the provider's
///     capabilities). "Connected" state is mirrored in UiPreferences.
/// </summary>
public sealed partial class SettingsWindow
{
    private bool _providerCardsBuilt;

    /// <summary>Resolved once per build from the host's ActualTheme. We must NOT
    /// pull theme brushes from Application.Current.Resources in code — those
    /// don't track a window whose theme is force-set (the user pins Light/Dark
    /// in Settings), so e.g. white "primary text" lands on a light card and is
    /// invisible. Reading ActualTheme + explicit light/dark hex is bulletproof.</summary>
    private bool _dark;

    // Capability accent colours — readable on light + dark tints alike.
    private const string SttColor = "#2FB37A";   // speech-to-text
    private const string LlmColor = "#6472FF";   // rewrite
    private const string RecapColor = "#C766E6"; // recap
    private const string OkColor = "#2FB37A";    // connected / ready

    /// <summary>Build the provider cards once, lazily, when the page is first
    /// shown. Re-entrant-safe.</summary>
    private void EnsureProviderCards()
    {
        if (_providerCardsBuilt) return;
        _providerCardsBuilt = true;
        BuildProviderCards();
    }

    private void BuildProviderCards()
    {
        if (ProviderCardsHost is null) return;
        // Resolve theme from the SAVED preference (the source of truth the rest
        // of the app uses), NOT ActualTheme — a force-set Light/Dark theme may
        // not have propagated to this lazily-built host yet, which made the
        // model-list container render near-black on a light window.
        _dark = UiPreferences.Load().Theme switch
        {
            "Dark" => true,
            "Light" => false,
            _ => ProviderCardsHost.ActualTheme == ElementTheme.Dark, // "Default" = follow system
        };
        ProviderCardsHost.Children.Clear();

        SeedConnectedFromCurrentConfig();

        foreach (var p in ProviderCatalog.All)
            ProviderCardsHost.Children.Add(BuildProviderCard(p));
    }

    /// <summary>On first run, mark whichever providers the current STT/LLM URLs
    /// point at as connected, so existing setups light up without re-entering
    /// keys. Idempotent — only seeds when the list is empty.</summary>
    private void SeedConnectedFromCurrentConfig()
    {
        var prefs = UiPreferences.Load();
        if (prefs.ConnectedProviders.Count > 0) return;

        void SeedFromUrl(string? url)
        {
            if (string.IsNullOrWhiteSpace(url)) return;
            var match = ProviderCatalog.All.FirstOrDefault(p =>
                !string.IsNullOrEmpty(p.Id) && p.Id != "custom" && p.Id != "local"
                && url!.Contains(p.Id, StringComparison.OrdinalIgnoreCase));
            if (match != null && !prefs.ConnectedProviders.Contains(match.Id))
                prefs.ConnectedProviders.Add(match.Id);
        }
        SeedFromUrl(ViewModel.ApiUrl);
        SeedFromUrl(ViewModel.LlmApiUrl);
        if (!prefs.ConnectedProviders.Contains("local"))
            prefs.ConnectedProviders.Add("local");
        prefs.Save();
    }

    private bool IsProviderConnected(string id) =>
        id == "local" || UiPreferences.Load().ConnectedProviders.Contains(id);

    // ── Card construction ────────────────────────────────────────────────

    private Expander BuildProviderCard(ProviderInfo p)
    {
        bool connected = IsProviderConnected(p.Id);

        var card = new Expander
        {
            HorizontalAlignment = HorizontalAlignment.Stretch,
            HorizontalContentAlignment = HorizontalAlignment.Stretch,
            IsExpanded = false, // all collapsed by default — the header tells the story
        };

        card.Header = BuildHeader(p, connected);
        card.Content = BuildBody(p, connected);
        return card;
    }

    /// <summary>Header: [logo tile] [name + sub-status] … [status pill].
    /// Three columns so the pill is right-aligned and the name block flexes,
    /// everything centred on one baseline.</summary>
    private FrameworkElement BuildHeader(ProviderInfo p, bool connected)
    {
        var grid = new Grid { ColumnSpacing = 14, MinHeight = 40 };
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        var tile = MakeLogoTile(p);
        Grid.SetColumn(tile, 0);
        grid.Children.Add(tile);

        var meta = new StackPanel { VerticalAlignment = VerticalAlignment.Center, Spacing = 2 };
        meta.Children.Add(new TextBlock
        {
            Text = p.Name,
            FontWeight = FontWeights.SemiBold,
            FontSize = 15,
            Foreground = ThemeBrush("TextFillColorPrimaryBrush"),
        });
        meta.Children.Add(new TextBlock
        {
            Text = SubStatusFor(p, connected),
            FontSize = 12.5,
            Foreground = ThemeBrush("TextFillColorSecondaryBrush"),
        });
        Grid.SetColumn(meta, 1);
        grid.Children.Add(meta);

        var pill = MakeStatusPill(p, connected);
        Grid.SetColumn(pill, 2);
        grid.Children.Add(pill);

        return grid;
    }

    /// <summary>Body: key entry + deep-link (or the on-device blurb), then the
    /// model list. Single column, consistent 14 px rhythm.</summary>
    private FrameworkElement BuildBody(ProviderInfo p, bool connected)
    {
        var body = new StackPanel { Spacing = 14, Padding = new Thickness(0, 6, 0, 2) };

        if (p.Id == "local")
        {
            body.Children.Add(new TextBlock
            {
                Text = "Runs entirely on your machine — private, offline, and free. No API key needed. Pick the exact on-device model on the Voice input and Output pages.",
                TextWrapping = TextWrapping.Wrap,
                FontSize = 13,
                Foreground = ThemeBrush("TextFillColorSecondaryBrush"),
            });
        }
        else if (p.IsKeyableHere())
        {
            body.Children.Add(BuildKeyRow(p, connected));
            body.Children.Add(BuildGetKeyRow(p));
        }
        else
        {
            // Deepgram (STT-only) and Custom can't be keyed via the per-provider
            // FFI — their key lives on the Voice input / Output page. Be honest
            // rather than showing a Connect button that silently does nothing.
            body.Children.Add(new TextBlock
            {
                Text = p.Id == "deepgram"
                    ? "Deepgram is speech-to-text only — enter its API key on the Voice input page (Speech-to-text → Cloud → Deepgram)."
                    : "Set a custom OpenAI-compatible endpoint + key on the Voice input page (STT) or Output page (rewrite).",
                TextWrapping = TextWrapping.Wrap,
                FontSize = 13,
                Foreground = ThemeBrush("TextFillColorSecondaryBrush"),
            });
            body.Children.Add(BuildGetKeyRow(p));
        }

        body.Children.Add(SectionCaption("MODELS"));
        body.Children.Add(BuildModelsList(p));
        return body;
    }

    private FrameworkElement BuildKeyRow(ProviderInfo p, bool connected)
    {
        var row = new Grid { ColumnSpacing = 8 };
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        var box = new PasswordBox
        {
            PlaceholderText = connected ? "Key saved — paste a new one to replace" : "Paste your API key…",
            Tag = p.Id,
            VerticalAlignment = VerticalAlignment.Center,
        };
        Grid.SetColumn(box, 0);
        row.Children.Add(box);

        var btn = new Button
        {
            Content = connected ? "Replace" : "Connect",
            VerticalAlignment = VerticalAlignment.Center,
            Style = connected ? null : (Style)Application.Current.Resources["AccentButtonStyle"],
        };
        btn.Click += (_, _) => ProviderConnectClicked(p, box);
        Grid.SetColumn(btn, 1);
        row.Children.Add(btn);

        if (connected)
        {
            var stack = new StackPanel { Spacing = 6 };
            stack.Children.Add(row);
            var remove = new HyperlinkButton { Content = "Remove key", Padding = new Thickness(0) };
            remove.Click += (_, _) => ProviderRemoveClicked(p);
            stack.Children.Add(remove);
            return stack;
        }
        return row;
    }

    private FrameworkElement BuildGetKeyRow(ProviderInfo p)
    {
        var stack = new StackPanel { Spacing = 2 };
        if (!string.IsNullOrEmpty(p.ConsoleUrl))
        {
            var get = new HyperlinkButton
            {
                Content = $"Get a {p.Name} key →",
                NavigateUri = SafeUri(p.ConsoleUrl),
                Padding = new Thickness(0),
            };
            stack.Children.Add(get);
        }
        stack.Children.Add(new TextBlock
        {
            Text = p.GetKeyHint,
            TextWrapping = TextWrapping.Wrap,
            FontSize = 12,
            Foreground = ThemeBrush("TextFillColorSecondaryBrush"),
        });
        return stack;
    }

    /// <summary>The model list — a bordered card with one row per model and a
    /// hairline divider between them, so it reads as a list, not a clump.</summary>
    private FrameworkElement BuildModelsList(ProviderInfo p)
    {
        var stack = new StackPanel();
        for (int i = 0; i < p.Models.Count; i++)
        {
            if (i > 0)
                stack.Children.Add(new Border
                {
                    Height = 1,
                    Margin = new Thickness(12, 0, 12, 0),
                    Background = ThemeBrush("DividerStrokeColorDefaultBrush"),
                });
            stack.Children.Add(BuildModelRow(p.Models[i]));
        }

        return new Border
        {
            CornerRadius = new CornerRadius(8),
            Background = ThemeBrush("CardBackgroundFillColorSecondaryBrush"),
            BorderBrush = ThemeBrush("ControlStrokeColorDefaultBrush"),
            BorderThickness = new Thickness(1),
            Child = stack,
        };
    }

    private FrameworkElement BuildModelRow(ProviderModel m)
    {
        var row = new Grid { ColumnSpacing = 10, Padding = new Thickness(12, 9, 12, 9) };
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        var name = new TextBlock
        {
            Text = m.Name,
            FontSize = 13.5,
            TextWrapping = TextWrapping.Wrap,
            VerticalAlignment = VerticalAlignment.Center,
            Foreground = ThemeBrush("TextFillColorPrimaryBrush"),
        };
        Grid.SetColumn(name, 0);
        row.Children.Add(name);

        var badges = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            Spacing = 5,
            VerticalAlignment = VerticalAlignment.Center,
        };
        if (m.Stt) badges.Children.Add(MakeBadge("STT", SttColor));
        if (m.Llm) badges.Children.Add(MakeBadge("LLM", LlmColor));
        if (m.Recap) badges.Children.Add(MakeBadge("Recap", RecapColor));
        Grid.SetColumn(badges, 1);
        row.Children.Add(badges);
        return row;
    }

    // ── Handlers ─────────────────────────────────────────────────────────

    private void ProviderConnectClicked(ProviderInfo p, PasswordBox box)
    {
        var key = box.Password?.Trim() ?? "";
        if (string.IsNullOrEmpty(key)) { box.Focus(FocusState.Programmatic); return; }

        // Save the one key into every scope the keystore FFI accepts for this
        // vendor (llm + recap). The FFI rejects the "stt" scope, so an STT key
        // for a provider used for speech still comes from the Voice input page.
        var scopes = p.KeySaveScopes();
        int worst = 0;
        foreach (var scope in scopes)
            worst = Math.Min(worst, SaveProviderKey(scope, p.Id, key));

        if (scopes.Count > 0 && worst == 0)
        {
            box.Password = "";
            MarkConnected(p.Id, true);
            App.Log($"[Providers] connected {p.Id}", "Providers");
        }
        else
        {
            App.Log($"[Providers] connect {p.Id} failed rc={worst}", "Providers");
        }
        BuildProviderCards();
    }

    private void ProviderRemoveClicked(ProviderInfo p)
    {
        if (p.Stt) SaveProviderKey("stt", p.Id, "");
        if (p.Llm) { SaveProviderKey("llm", p.Id, ""); SaveProviderKey("recap", p.Id, ""); }
        MarkConnected(p.Id, false);
        App.Log($"[Providers] removed {p.Id}", "Providers");
        BuildProviderCards();
    }

    private static int SaveProviderKey(string scope, string provider, string key)
    {
        try { return Interop.DimmyNative.dimmy_save_llm_provider_key(scope, provider, key); }
        catch (Exception ex) { App.Log($"[Providers] save threw: {ex.Message}", "Providers"); return -1; }
    }

    private static void MarkConnected(string id, bool connected)
    {
        var prefs = UiPreferences.Load();
        if (connected)
        {
            if (!prefs.ConnectedProviders.Contains(id)) prefs.ConnectedProviders.Add(id);
        }
        else
        {
            prefs.ConnectedProviders.RemoveAll(x => x == id);
        }
        prefs.Save();
    }

    // ── Small visual helpers ─────────────────────────────────────────────

    /// <summary>White rounded tile holding the real brand SVG. White on purpose
    /// (favicon-tile convention) so monochrome currentColor marks render dark
    /// and stay visible in both themes; colour marks sit cleanly on it too.</summary>
    private FrameworkElement MakeLogoTile(ProviderInfo p)
    {
        var img = new Image
        {
            Width = 22,
            Height = 22,
            HorizontalAlignment = HorizontalAlignment.Center,
            VerticalAlignment = VerticalAlignment.Center,
            Source = new SvgImageSource(SafeUri($"ms-appx:///Assets/Providers/{p.Id}.svg")),
        };
        return new Border
        {
            Width = 40,
            Height = 40,
            CornerRadius = new CornerRadius(11),
            Background = SolidBrush("#FFFFFF"),
            BorderBrush = ThemeBrush("ControlStrokeColorSecondaryBrush"),
            BorderThickness = new Thickness(1),
            VerticalAlignment = VerticalAlignment.Center,
            Child = img,
        };
    }

    private FrameworkElement MakeStatusPill(ProviderInfo p, bool connected)
    {
        bool ok = connected || p.Id == "local";
        string label = p.Id == "local" ? "Ready" : connected ? "Connected" : "Add key";

        var inner = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            Spacing = 6,
            VerticalAlignment = VerticalAlignment.Center,
        };
        if (ok)
            inner.Children.Add(new Ellipse
            {
                Width = 7,
                Height = 7,
                Fill = SolidBrush(OkColor),
                VerticalAlignment = VerticalAlignment.Center,
            });
        inner.Children.Add(new TextBlock
        {
            Text = label,
            FontSize = 11.5,
            FontWeight = FontWeights.Medium,
            Foreground = ok ? SolidBrush(OkColor) : ThemeBrush("TextFillColorSecondaryBrush"),
            VerticalAlignment = VerticalAlignment.Center,
        });

        return new Border
        {
            CornerRadius = new CornerRadius(5),
            Padding = new Thickness(8, 3, 9, 3),
            VerticalAlignment = VerticalAlignment.Center,
            Background = ok ? SolidBrush(WithAlpha(OkColor, 0x22)) : ThemeBrush("ControlFillColorSecondaryBrush"),
            BorderBrush = ok ? SolidBrush(WithAlpha(OkColor, 0x55)) : ThemeBrush("ControlStrokeColorDefaultBrush"),
            BorderThickness = new Thickness(1),
            Child = inner,
        };
    }

    private static Border MakeBadge(string text, string colorHex)
    {
        return new Border
        {
            CornerRadius = new CornerRadius(5),
            Padding = new Thickness(7, 2, 7, 2),
            Background = SolidBrush(WithAlpha(colorHex, 0x22)),
            BorderBrush = SolidBrush(WithAlpha(colorHex, 0x44)),
            BorderThickness = new Thickness(1),
            Child = new TextBlock
            {
                Text = text,
                FontSize = 10,
                FontWeight = FontWeights.SemiBold,
                Foreground = SolidBrush(colorHex),
            },
        };
    }

    private TextBlock SectionCaption(string text) => new()
    {
        Text = text,
        FontSize = 11,
        FontWeight = FontWeights.SemiBold,
        CharacterSpacing = 60,
        Foreground = ThemeBrush("TextFillColorSecondaryBrush"),
    };

    private string SubStatusFor(ProviderInfo p, bool connected)
    {
        if (p.Id == "local") return "On-device · no key needed";
        int n = p.Models.Count;
        if (connected) return $"Connected · {n} model{(n == 1 ? "" : "s")}";
        return p.Stt && p.Llm ? "Speech + rewrite · API key required"
            : p.Stt ? "Speech-to-text · API key required"
            : "Rewrite & recap · API key required";
    }

    // ── Colour / URI plumbing ────────────────────────────────────────────

    /// <summary>Theme-correct brush for a logical role, resolved from the host's
    /// ActualTheme (NOT from app resources, which don't track a force-set
    /// window theme). Explicit light/dark hex per role so text always reads.</summary>
    private SolidColorBrush ThemeBrush(string key) => key switch
    {
        "TextFillColorPrimaryBrush" => SolidBrush(_dark ? "#F2F2F3" : "#1B1B1B"),
        "TextFillColorSecondaryBrush" => SolidBrush(_dark ? "#B4B4B8" : "#5B5B61"),
        "CardBackgroundFillColorSecondaryBrush" => SolidBrush(_dark ? "#2B2B30" : "#F6F6F8"),
        "ControlStrokeColorDefaultBrush" => SolidBrush(_dark ? "#3A3A40" : "#E4E4E7"),
        "ControlStrokeColorSecondaryBrush" => SolidBrush(_dark ? "#48484F" : "#E0E0E3"),
        "DividerStrokeColorDefaultBrush" => SolidBrush(_dark ? "#37373C" : "#EAEAEE"),
        "ControlFillColorSecondaryBrush" => SolidBrush(_dark ? "#34343B" : "#F0F0F3"),
        _ => SolidBrush(_dark ? "#B4B4B8" : "#5B5B61"),
    };

    private static Uri SafeUri(string url)
    {
        try { return new Uri(url); } catch { return new Uri("https://dimmy.app"); }
    }

    private static SolidColorBrush SolidBrush(string hex) => new(ParseHex(hex));

    /// <summary>Parse #RGB / #RRGGBB / #AARRGGBB into a colour. Falls back to a
    /// neutral grey on any malformed input so the UI never throws while building.</summary>
    private static WinColor ParseHex(string hex)
    {
        try
        {
            hex = hex.TrimStart('#');
            byte a = 0xFF, r, g, b;
            if (hex.Length == 8)
            {
                a = Convert.ToByte(hex.Substring(0, 2), 16);
                r = Convert.ToByte(hex.Substring(2, 2), 16);
                g = Convert.ToByte(hex.Substring(4, 2), 16);
                b = Convert.ToByte(hex.Substring(6, 2), 16);
            }
            else if (hex.Length == 6)
            {
                r = Convert.ToByte(hex.Substring(0, 2), 16);
                g = Convert.ToByte(hex.Substring(2, 2), 16);
                b = Convert.ToByte(hex.Substring(4, 2), 16);
            }
            else { return WinColor.FromArgb(0xFF, 0x9A, 0xA0, 0xAC); }
            return WinColor.FromArgb(a, r, g, b);
        }
        catch { return WinColor.FromArgb(0xFF, 0x9A, 0xA0, 0xAC); }
    }

    /// <summary>#RRGGBB + alpha byte → #AARRGGBB string for the badge/pill tints.</summary>
    private static string WithAlpha(string hex, byte alpha)
    {
        hex = hex.TrimStart('#');
        if (hex.Length != 6) return "#" + hex;
        return $"#{alpha:X2}{hex}";
    }
}

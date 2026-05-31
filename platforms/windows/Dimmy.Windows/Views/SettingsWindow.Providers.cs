using System;
using System.Linq;
using Dimmy.Windows.Services;
using Microsoft.UI.Text;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using WinColor = global::Windows.UI.Color;

namespace Dimmy.Windows.Views;

/// <summary>
/// Providers &amp; keys page — built programmatically (no XAML DataTemplates) so
/// there is zero XAML-compiler surface. One card per provider from
/// <see cref="ProviderCatalog"/>: status, key entry, a deep-link "Get key →"
/// wizard to the exact console, and capability badges per model.
///
/// Backend is UNCHANGED: keys go through dimmy_save_llm_provider_key into the
/// existing AES-256 keystore (scopes stt/llm/recap per the provider's
/// capabilities). "Connected" state is mirrored in UiPreferences for display.
/// The per-task routing (which provider does STT/LLM/Recap) keeps using the
/// existing config fields via the existing pages — this page is the clean
/// one-key-per-provider entry surface that was the user's #1 pain point.
/// </summary>
public sealed partial class SettingsWindow
{
    private bool _providerCardsBuilt;

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
        // On-device is always "connected" — no key needed.
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
            IsExpanded = !connected && p.Id != "local", // open the ones that need a key
        };

        // ── Header: mark chip + name + status pill + sub-line ──
        var header = new Grid { ColumnSpacing = 12 };
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });

        var mark = MakeMarkChip(p);
        Grid.SetColumn(mark, 0);
        header.Children.Add(mark);

        var meta = new StackPanel { VerticalAlignment = VerticalAlignment.Center, Spacing = 1 };
        var nameRow = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
        nameRow.Children.Add(new TextBlock { Text = p.Name, FontWeight = FontWeights.SemiBold, FontSize = 15 });
        nameRow.Children.Add(MakeStatusPill(p, connected));
        meta.Children.Add(nameRow);
        meta.Children.Add(new TextBlock
        {
            Text = SubStatusFor(p, connected),
            FontSize = 12.5,
            Foreground = ThemeBrush("TextFillColorSecondaryBrush"),
        });
        Grid.SetColumn(meta, 1);
        header.Children.Add(meta);
        card.Header = header;

        // ── Body ──
        var body = new StackPanel { Spacing = 12, Padding = new Thickness(0, 4, 0, 2) };

        if (p.Id == "local")
        {
            body.Children.Add(new TextBlock
            {
                Text = "Runs entirely on your machine — private and free. No API key required. Pick the on-device model on the Voice input and Output pages.",
                TextWrapping = TextWrapping.Wrap,
                FontSize = 13,
                Foreground = ThemeBrush("TextFillColorSecondaryBrush"),
            });
        }
        else
        {
            body.Children.Add(BuildKeyRow(p, connected));
            body.Children.Add(BuildGetKeyRow(p));
        }

        // Models + capability badges (the educational "what can it do").
        var mlabel = new TextBlock
        {
            Text = "MODELS",
            FontSize = 11,
            FontWeight = FontWeights.SemiBold,
            CharacterSpacing = 60,
            Margin = new Thickness(0, 4, 0, 0),
            Foreground = ThemeBrush("TextFillColorSecondaryBrush"),
        };
        body.Children.Add(mlabel);
        foreach (var m in p.Models)
            body.Children.Add(BuildModelRow(m));

        card.Content = body;
        return card;
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
        };
        Grid.SetColumn(box, 0);
        row.Children.Add(box);

        var btn = new Button
        {
            Content = connected ? "Replace" : "Connect",
            Style = connected ? null : (Style)Application.Current.Resources["AccentButtonStyle"],
        };
        btn.Click += (_, _) => ProviderConnectClicked(p, box);
        Grid.SetColumn(btn, 1);
        row.Children.Add(btn);

        if (connected)
        {
            // A "Remove" affordance lives under the field so it isn't a fat
            // finger away from Replace.
            var stack = new StackPanel { Spacing = 8 };
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
        var stack = new StackPanel { Spacing = 4 };
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

    private FrameworkElement BuildModelRow(ProviderModel m)
    {
        var row = new Grid { ColumnSpacing = 10, Padding = new Thickness(0, 6, 0, 6) };
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        var name = new TextBlock { Text = m.Name, FontSize = 13.5, VerticalAlignment = VerticalAlignment.Center };
        Grid.SetColumn(name, 0);
        row.Children.Add(name);

        var badges = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 5, VerticalAlignment = VerticalAlignment.Center };
        if (m.Stt) badges.Children.Add(MakeBadge("STT", "#46D4A0"));
        if (m.Llm) badges.Children.Add(MakeBadge("LLM", "#7C8CFF"));
        if (m.Recap) badges.Children.Add(MakeBadge("Recap", "#E07CFF"));
        Grid.SetColumn(badges, 1);
        row.Children.Add(badges);
        return row;
    }

    // ── Handlers ─────────────────────────────────────────────────────────

    private void ProviderConnectClicked(ProviderInfo p, PasswordBox box)
    {
        var key = box.Password?.Trim() ?? "";
        if (string.IsNullOrEmpty(key)) { box.Focus(FocusState.Programmatic); return; }

        int worst = 0;
        // Save the one key into every scope the provider can serve, so
        // whichever capability dispatches to it finds the key. Harmless extra
        // entries for capabilities the provider can't do.
        if (p.Stt) worst = Math.Min(worst, SaveProviderKey("stt", p.Id, key));
        if (p.Llm)
        {
            worst = Math.Min(worst, SaveProviderKey("llm", p.Id, key));
            worst = Math.Min(worst, SaveProviderKey("recap", p.Id, key));
        }

        if (worst == 0)
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

    private static Border MakeMarkChip(ProviderInfo p)
    {
        return new Border
        {
            Width = 34,
            Height = 34,
            CornerRadius = new CornerRadius(9),
            VerticalAlignment = VerticalAlignment.Center,
            Background = SolidBrush("#1B1E25"),
            BorderBrush = SolidBrush("#262A33"),
            BorderThickness = new Thickness(1),
            Child = new TextBlock
            {
                Text = p.Mark,
                FontWeight = FontWeights.Bold,
                FontSize = 14,
                Foreground = SolidBrush(p.AccentHex),
                HorizontalAlignment = HorizontalAlignment.Center,
                VerticalAlignment = VerticalAlignment.Center,
            },
        };
    }

    private Border MakeStatusPill(ProviderInfo p, bool connected)
    {
        var (txt, fg, bg) = connected
            ? ("connected", "#46D4A0", "#1A46D4A0")
            : (p.Id == "local" ? ("ready", "#46D4A0", "#1A46D4A0") : ("no key", "#9AA0AC", "#14FFFFFF"));
        return new Border
        {
            CornerRadius = new CornerRadius(20),
            Padding = new Thickness(7, 2, 7, 2),
            VerticalAlignment = VerticalAlignment.Center,
            Background = SolidBrush(bg),
            Child = new TextBlock
            {
                Text = txt,
                FontSize = 10.5,
                FontWeight = FontWeights.Medium,
                Foreground = SolidBrush(fg),
            },
        };
    }

    private static Border MakeBadge(string text, string colorHex)
    {
        return new Border
        {
            CornerRadius = new CornerRadius(5),
            Padding = new Thickness(7, 2, 7, 2),
            Background = SolidBrush(WithAlpha(colorHex, 0x14)),
            BorderBrush = SolidBrush(WithAlpha(colorHex, 0x33)),
            BorderThickness = new Thickness(1),
            Child = new TextBlock
            {
                Text = text,
                FontSize = 10,
                FontWeight = FontWeights.Medium,
                Foreground = SolidBrush(colorHex),
            },
        };
    }

    private string SubStatusFor(ProviderInfo p, bool connected)
    {
        if (p.Id == "local") return "On-device · no key needed";
        if (connected) return $"{p.Models.Count} model{(p.Models.Count == 1 ? "" : "s")} available";
        return "API key required";
    }

    private static SolidColorBrush ThemeBrushOrNull(string key) =>
        Application.Current.Resources.TryGetValue(key, out var v) && v is SolidColorBrush b ? b : null!;

    private SolidColorBrush ThemeBrush(string key)
    {
        var b = ThemeBrushOrNull(key);
        return b ?? SolidBrush("#9AA0AC");
    }

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

    /// <summary>#RRGGBB + alpha byte → #AARRGGBB string for the badge tints.</summary>
    private static string WithAlpha(string hex, byte alpha)
    {
        hex = hex.TrimStart('#');
        if (hex.Length != 6) return "#" + hex;
        return $"#{alpha:X2}{hex}";
    }
}

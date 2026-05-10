using System;
using System.Collections.Generic;
using System.Text;
using System.Text.Json;
using System.Threading.Tasks;
using Dimmy.Windows.Interop;

namespace Dimmy.Windows.Services;

/// <summary>
/// Thin C# wrapper over the Notion FFI exports in <see cref="DimmyNative"/>.
///
/// All actual REST work happens in Rust (`core/src/notion.rs`); this
/// class just marshals strings + parses the JSON envelopes. Async
/// methods <c>Task.Run</c> the FFI call so the UI thread stays free —
/// each call hits a real HTTPS endpoint and can take 200 ms-2 s.
/// </summary>
public static class NotionService
{
    /// <summary>One result from Notion search — page or database the
    /// integration has access to.</summary>
    public sealed record SearchResult(
        string Id, string Object, string Title, string ParentLabel, string Url);

    /// <summary>Connection status returned by <see cref="TestConnectionAsync"/>.</summary>
    public sealed record ConnectionResult(
        bool Ok, string BotName, string WorkspaceName, string? Error);

    /// <summary>Result of a recap upload to Notion.</summary>
    public sealed record SendRecapResult(
        bool Ok, string PageId, string PageUrl, string? Error);

    private const int Buf = 1 << 14; // 16 KB — comfortably larger than
                                     // any expected envelope (workspace
                                     // info ~200 B, search 100 results
                                     // ~10 KB worst case).

    public static int SetToken(string token)
        => DimmyNative.dimmy_notion_set_token(token ?? string.Empty);

    public static bool HasToken()
        => DimmyNative.dimmy_notion_has_token() == 1;

    public static async Task<ConnectionResult> TestConnectionAsync()
    {
        var (json, err) = await Task.Run(() =>
        {
            var buf = new byte[Buf];
            int n = DimmyNative.dimmy_notion_test_connection(buf, buf.Length);
            return n switch
            {
                < 0 => (string.Empty, "FFI invalid args / no token"),
                _ => (Encoding.UTF8.GetString(buf, 0, n), null!),
            };
        });
        if (err != null) return new ConnectionResult(false, "", "", err);
        try
        {
            using var doc = JsonDocument.Parse(json);
            var root = doc.RootElement;
            if (root.TryGetProperty("ok", out var okEl) && okEl.GetBoolean())
            {
                return new ConnectionResult(
                    true,
                    root.GetProperty("bot_name").GetString() ?? "",
                    root.GetProperty("workspace_name").GetString() ?? "",
                    null);
            }
            var errMsg = root.TryGetProperty("error", out var errEl)
                ? errEl.GetString() ?? "Unknown error"
                : "Unknown error";
            return new ConnectionResult(false, "", "", errMsg);
        }
        catch (JsonException ex)
        {
            return new ConnectionResult(false, "", "", $"Invalid response JSON: {ex.Message}");
        }
    }

    public static async Task<(IReadOnlyList<SearchResult> Results, string? Error)> SearchAsync(string query)
    {
        var (json, err) = await Task.Run(() =>
        {
            var buf = new byte[Buf];
            int n = DimmyNative.dimmy_notion_search(query ?? string.Empty, buf, buf.Length);
            return n switch
            {
                < 0 => (string.Empty, "FFI invalid args / no token"),
                _ => (Encoding.UTF8.GetString(buf, 0, n), null!),
            };
        });
        if (err != null) return (Array.Empty<SearchResult>(), err);
        try
        {
            using var doc = JsonDocument.Parse(json);
            var root = doc.RootElement;
            // Error envelope shape: {"error":"..."}
            if (root.ValueKind == JsonValueKind.Object && root.TryGetProperty("error", out var errEl))
            {
                return (Array.Empty<SearchResult>(), errEl.GetString() ?? "Unknown error");
            }
            // Success: an array of result objects.
            var list = new List<SearchResult>();
            if (root.ValueKind == JsonValueKind.Array)
            {
                foreach (var item in root.EnumerateArray())
                {
                    list.Add(new SearchResult(
                        item.TryGetProperty("id", out var id) ? id.GetString() ?? "" : "",
                        item.TryGetProperty("object", out var obj) ? obj.GetString() ?? "" : "",
                        item.TryGetProperty("title", out var t) ? t.GetString() ?? "" : "",
                        item.TryGetProperty("parent_label", out var pl) ? pl.GetString() ?? "" : "",
                        item.TryGetProperty("url", out var u) ? u.GetString() ?? "" : ""));
                }
            }
            return (list, null);
        }
        catch (JsonException ex)
        {
            return (Array.Empty<SearchResult>(), $"Invalid response JSON: {ex.Message}");
        }
    }

    public static async Task<SendRecapResult> SendRecapAsync(string meetingDir)
    {
        if (string.IsNullOrEmpty(meetingDir))
            return new SendRecapResult(false, "", "", "Empty meeting dir");
        var (json, err) = await Task.Run(() =>
        {
            var buf = new byte[Buf];
            int n = DimmyNative.dimmy_notion_send_recap(meetingDir, buf, buf.Length);
            return n switch
            {
                < 0 => (string.Empty, "FFI invalid args"),
                _ => (Encoding.UTF8.GetString(buf, 0, n), null!),
            };
        });
        if (err != null) return new SendRecapResult(false, "", "", err);
        try
        {
            using var doc = JsonDocument.Parse(json);
            var root = doc.RootElement;
            if (root.TryGetProperty("ok", out var okEl) && okEl.GetBoolean())
            {
                return new SendRecapResult(
                    true,
                    root.TryGetProperty("page_id", out var id) ? id.GetString() ?? "" : "",
                    root.TryGetProperty("page_url", out var url) ? url.GetString() ?? "" : "",
                    null);
            }
            var errMsg = root.TryGetProperty("error", out var e) ? e.GetString() ?? "Unknown error" : "Unknown error";
            return new SendRecapResult(false, "", "", errMsg);
        }
        catch (JsonException ex)
        {
            return new SendRecapResult(false, "", "", $"Invalid response JSON: {ex.Message}");
        }
    }
}

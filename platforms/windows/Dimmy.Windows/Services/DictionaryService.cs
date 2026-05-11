using System;
using System.Collections.Generic;
using System.Text;
using System.Text.Json;
using Dimmy.Windows.Interop;

namespace Dimmy.Windows.Services;

/// <summary>
/// Thin C# wrapper over the Rust user-dictionary FFI exports. The Rust
/// side persists to <c>config.json</c> automatically on add / remove,
/// and `compose_stt_prompt` injects the dict into every STT call
/// (cloud + local Whisper). Parakeet local backend doesn't honour the
/// dict — see core/src/lib.rs notes.
///
/// Static stateless wrapper — no caching here. The dictionary lives in
/// the Rust AppState; each method round-trips. Cheap (sub-ms) so we
/// don't need to memoise.
/// </summary>
public static class DictionaryService
{
    private const int Buf = 8 * 1024;

    /// <summary>Append <paramref name="word"/> to the user dictionary.
    /// Returns:
    ///   <list type="bullet">
    ///     <item>0 — added (new word)</item>
    ///     <item>1 — already present (case-insensitive)</item>
    ///     <item>-1 — invalid input / persistence failure</item>
    ///   </list>
    /// Empty / whitespace-only input is rejected with -1 by the Rust
    /// side; callers should trim before calling.</summary>
    public static int Add(string word)
    {
        if (string.IsNullOrWhiteSpace(word)) return -1;
        return DimmyNative.dimmy_user_dict_add(word.Trim());
    }

    /// <summary>Remove all entries matching <paramref name="word"/>
    /// case-insensitively. Returns the count dropped (0 = no match)
    /// or -1 on failure.</summary>
    public static int Remove(string word)
    {
        if (string.IsNullOrWhiteSpace(word)) return -1;
        return DimmyNative.dimmy_user_dict_remove(word.Trim());
    }

    /// <summary>Read the current dictionary as a list of strings.
    /// Returns an empty list on FFI failure (the dict simply isn't
    /// populated) — callers don't need to differentiate.</summary>
    public static IReadOnlyList<string> List()
    {
        var buf = new byte[Buf];
        int n = DimmyNative.dimmy_user_dict_list_json(buf, buf.Length);
        if (n <= 0) return Array.Empty<string>();
        try
        {
            var json = Encoding.UTF8.GetString(buf, 0, n);
            var arr = JsonSerializer.Deserialize<List<string>>(json);
            return arr ?? new List<string>();
        }
        catch (JsonException)
        {
            return Array.Empty<string>();
        }
    }
}

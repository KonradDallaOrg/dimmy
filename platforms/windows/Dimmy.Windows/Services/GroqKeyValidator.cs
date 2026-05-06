using System;
using System.Diagnostics;
using System.Net;
using System.Net.Http;
using System.Net.Http.Headers;
using System.Threading;
using System.Threading.Tasks;

namespace Dimmy.Windows.Services;

public sealed record GroqKeyValidationResult(bool IsValid, string? Error)
{
    public static GroqKeyValidationResult Ok() => new(true, null);
    public static GroqKeyValidationResult Invalid(string error) => new(false, error);
}

// One-shot Groq API key validator. Pings /models with Bearer auth.
public static class GroqKeyValidator
{
    private const string ModelsEndpoint = "https://api.groq.com/openai/v1/models";
    private static readonly TimeSpan Timeout = TimeSpan.FromSeconds(8);

    private static readonly HttpClient _http = new()
    {
        Timeout = Timeout,
    };

    static GroqKeyValidator()
    {
        _http.DefaultRequestHeaders.UserAgent.ParseAdd("Dimmy-Onboarding/1.0");
    }

    public static async Task<GroqKeyValidationResult> ValidateAsync(string apiKey, CancellationToken ct = default)
    {
        if (string.IsNullOrWhiteSpace(apiKey))
            return GroqKeyValidationResult.Invalid("Key is empty");

        try
        {
            using var req = new HttpRequestMessage(HttpMethod.Get, ModelsEndpoint);
            req.Headers.Authorization = new AuthenticationHeaderValue("Bearer", apiKey.Trim());

            using var resp = await _http.SendAsync(req, ct).ConfigureAwait(false);

            if (resp.IsSuccessStatusCode)
                return GroqKeyValidationResult.Ok();

            return resp.StatusCode switch
            {
                HttpStatusCode.Unauthorized => GroqKeyValidationResult.Invalid("Invalid key"),
                HttpStatusCode.Forbidden => GroqKeyValidationResult.Invalid("Key has no permissions"),
                HttpStatusCode.TooManyRequests => GroqKeyValidationResult.Invalid("Rate limited. Try again in a moment."),
                _ => GroqKeyValidationResult.Invalid($"Unexpected response ({(int)resp.StatusCode})"),
            };
        }
        catch (TaskCanceledException) when (!ct.IsCancellationRequested)
        {
            return GroqKeyValidationResult.Invalid("Timeout. Check connection.");
        }
        catch (OperationCanceledException)
        {
            throw;
        }
        catch (HttpRequestException ex)
        {
            Debug.WriteLine($"[Onboarding] Groq validation network error: {ex.Message}");
            return GroqKeyValidationResult.Invalid("Network error. Check connection.");
        }
        catch (Exception ex)
        {
            Debug.WriteLine($"[Onboarding] Groq validation threw: {ex.GetType().Name}: {ex.Message}");
            return GroqKeyValidationResult.Invalid("Validation failed");
        }
    }
}

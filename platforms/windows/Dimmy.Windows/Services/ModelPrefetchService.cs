using System;
using System.Diagnostics;
using System.IO;
using System.Net;
using System.Net.Http;
using System.Threading;
using System.Threading.Tasks;

namespace Dimmy.Windows.Services;

public enum ModelDownloadStatus
{
    NotStarted,
    Downloading,
    Ready,
    Failed,
    Cancelled,
    Offline,
}

public sealed record ModelDownloadState(
    ModelDownloadStatus BaseStatus,
    long BaseBytesDownloaded,
    long BaseTotalBytes,
    string? BaseError,
    ModelDownloadStatus SmallStatus,
    long SmallBytesDownloaded,
    long SmallTotalBytes,
    string? SmallError)
{
    public static readonly ModelDownloadState Initial = new(
        ModelDownloadStatus.NotStarted, 0, 0, null,
        ModelDownloadStatus.NotStarted, 0, 0, null);
}

// Downloads whisper models from HuggingFace into the folder Rust reads at startup.
// Uses the existing convention in ModelPaths — no FFI, no Rust changes.
//
// Lifecycle: owned by OnboardingWindow. StartBasePrefetch() is fire-and-forget.
// Dispose() cancels in-flight downloads; partial .part files are kept for resume.
public sealed class ModelPrefetchService : IDisposable
{
    private const int BufferSize = 64 * 1024;
    private const long SmallTriggerBytes = 15L * 1024 * 1024;
    private const double SmallTriggerMbPerSec = 6.0;

    private readonly HttpClient _http;
    private readonly CancellationTokenSource _cts = new();
    private readonly object _stateLock = new();
    private ModelDownloadState _state = ModelDownloadState.Initial;
    private Task? _baseTask;
    private Task? _smallTask;
    private bool _disposed;

    public event EventHandler<ModelDownloadState>? StateChanged;

    public ModelPrefetchService()
    {
        _http = new HttpClient
        {
            Timeout = Timeout.InfiniteTimeSpan, // stall detection happens via read cancellation
        };
        _http.DefaultRequestHeaders.UserAgent.ParseAdd("Dimmy-Onboarding/1.0");
    }

    public ModelDownloadState State
    {
        get { lock (_stateLock) return _state; }
    }

    public void StartBasePrefetch()
    {
        if (_disposed) return;
        lock (_stateLock)
        {
            if (_baseTask != null) return;
        }

        _baseTask = Task.Run(() => DownloadAsync(
            ModelPaths.BaseModelFilename,
            ModelPaths.BaseModelSizeBytes,
            isBase: true,
            _cts.Token));
    }

    /// Start downloading an arbitrary whisper model file. Used when the
    /// onboarding user picks something other than the default base —
    /// e.g. small / medium / large-turbo. Reuses the BaseStatus state
    /// slot so the existing UI binding (DownloadPercent / status text)
    /// reflects the active download regardless of which file. Single-
    /// flight: a second call while a download is in-flight is ignored.
    public void StartFor(string filename, long expectedSize)
    {
        if (_disposed) return;
        lock (_stateLock)
        {
            if (_baseTask != null) return;
            // Reset state so the UI doesn't briefly show stale bytes
            // from a prior run when the new file's totals differ.
            _state = ModelDownloadState.Initial;
        }
        NotifyStateChanged();
        _baseTask = Task.Run(() => DownloadAsync(
            filename, expectedSize, isBase: true, _cts.Token));
    }

    public void Retry()
    {
        lock (_stateLock)
        {
            if (_state.BaseStatus is ModelDownloadStatus.Failed or ModelDownloadStatus.Offline)
            {
                _baseTask = null;
                _state = _state with { BaseStatus = ModelDownloadStatus.NotStarted, BaseError = null };
            }
        }
        NotifyStateChanged();
        StartBasePrefetch();
    }

    private async Task DownloadAsync(string filename, long expectedSize, bool isBase, CancellationToken ct)
    {
        var finalPath = ModelPaths.GetModelFilePath(filename);
        var partPath = ModelPaths.GetPartFilePath(filename);

        try
        {
            Directory.CreateDirectory(ModelPaths.ModelsDirectory);

            if (File.Exists(finalPath) && new FileInfo(finalPath).Length >= expectedSize / 2)
            {
                UpdateState(isBase, ModelDownloadStatus.Ready, new FileInfo(finalPath).Length, new FileInfo(finalPath).Length, null);
                return;
            }

            UpdateState(isBase, ModelDownloadStatus.Downloading, 0, expectedSize, null);

            long resumeFrom = File.Exists(partPath) ? new FileInfo(partPath).Length : 0;

            using var req = new HttpRequestMessage(HttpMethod.Get, ModelPaths.GetDownloadUrl(filename));
            if (resumeFrom > 0)
                req.Headers.Range = new System.Net.Http.Headers.RangeHeaderValue(resumeFrom, null);

            using var resp = await _http.SendAsync(req, HttpCompletionOption.ResponseHeadersRead, ct).ConfigureAwait(false);

            // If server ignores Range and returns 200, start fresh
            if (resumeFrom > 0 && resp.StatusCode == HttpStatusCode.OK)
            {
                resumeFrom = 0;
                try { File.Delete(partPath); } catch { }
            }
            resp.EnsureSuccessStatusCode();

            long total = resp.Content.Headers.ContentLength.HasValue
                ? resp.Content.Headers.ContentLength.Value + resumeFrom
                : expectedSize;

            await using var netStream = await resp.Content.ReadAsStreamAsync(ct).ConfigureAwait(false);
            await using (var fs = new FileStream(partPath, FileMode.Append, FileAccess.Write, FileShare.None, BufferSize, useAsync: true))
            {
                var buffer = new byte[BufferSize];
                long downloaded = resumeFrom;
                var sw = Stopwatch.StartNew();
                long lastNotifyBytes = downloaded;
                bool smallTriggered = !isBase;

                while (true)
                {
                    int read = await netStream.ReadAsync(buffer.AsMemory(0, BufferSize), ct).ConfigureAwait(false);
                    if (read == 0) break;
                    await fs.WriteAsync(buffer.AsMemory(0, read), ct).ConfigureAwait(false);
                    downloaded += read;

                    if (downloaded - lastNotifyBytes >= 256 * 1024)
                    {
                        UpdateState(isBase, ModelDownloadStatus.Downloading, downloaded, total, null);
                        lastNotifyBytes = downloaded;
                    }

                    if (!smallTriggered && downloaded >= SmallTriggerBytes)
                    {
                        double mbPerSec = (downloaded - resumeFrom) / 1024.0 / 1024.0 / Math.Max(sw.Elapsed.TotalSeconds, 0.001);
                        if (mbPerSec >= SmallTriggerMbPerSec)
                        {
                            smallTriggered = true;
                            TryStartSmall();
                        }
                    }
                }
            }

            File.Move(partPath, finalPath, overwrite: true);
            UpdateState(isBase, ModelDownloadStatus.Ready, total, total, null);
            Debug.WriteLine($"[Onboarding] {filename} ready at {finalPath}");
        }
        catch (OperationCanceledException)
        {
            UpdateState(isBase, ModelDownloadStatus.Cancelled, 0, 0, null);
        }
        catch (HttpRequestException ex) when (ex.InnerException is System.Net.Sockets.SocketException)
        {
            UpdateState(isBase, ModelDownloadStatus.Offline, 0, expectedSize, "No internet connection");
            Debug.WriteLine($"[Onboarding] {filename} offline: {ex.Message}");
        }
        catch (Exception ex)
        {
            UpdateState(isBase, ModelDownloadStatus.Failed, 0, expectedSize, Truncate(ex.Message, 120));
            Debug.WriteLine($"[Onboarding] {filename} failed: {ex.GetType().Name}: {ex.Message}");
        }
    }

    private void TryStartSmall()
    {
        lock (_stateLock)
        {
            if (_smallTask != null) return;
        }
        _smallTask = Task.Run(() => DownloadAsync(
            ModelPaths.SmallModelFilename,
            ModelPaths.SmallModelSizeBytes,
            isBase: false,
            _cts.Token));
    }

    private void UpdateState(bool isBase, ModelDownloadStatus status, long done, long total, string? error)
    {
        lock (_stateLock)
        {
            _state = isBase
                ? _state with { BaseStatus = status, BaseBytesDownloaded = done, BaseTotalBytes = total, BaseError = error }
                : _state with { SmallStatus = status, SmallBytesDownloaded = done, SmallTotalBytes = total, SmallError = error };
        }
        NotifyStateChanged();
    }

    private void NotifyStateChanged()
    {
        var snapshot = State;
        try { StateChanged?.Invoke(this, snapshot); }
        catch (Exception ex) { Debug.WriteLine($"[Onboarding] StateChanged handler threw: {ex.Message}"); }
    }

    private static string Truncate(string s, int max) => s.Length <= max ? s : s[..max] + "...";

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        try { _cts.Cancel(); } catch { }
        _cts.Dispose();
        _http.Dispose();
    }
}

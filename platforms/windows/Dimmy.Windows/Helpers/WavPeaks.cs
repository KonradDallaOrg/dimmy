using System;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json;

namespace Dimmy.Windows.Helpers;

/// Minimal WAV reader that produces normalized peak buckets for
/// waveform rendering. Only handles the formats Dimmy itself writes
/// (16 kHz mono PCM_S16LE, 16 kHz mono PCM_F32LE) plus stereo PCM_S16LE
/// for arbitrary user-loaded files. Anything else returns an empty
/// array — caller falls back to "no waveform" UI.
///
/// Output: float[bucketCount] of values in [0..1] representing the
/// peak absolute amplitude of each bucket. The History detail panel
/// renders this as filled rectangles in a Canvas.
public static class WavPeaks
{
    /// Read peaks for ANY audio file the loader supports. WAV stays on
    /// the in-process hand-rolled fast path (no FFI hop, no Rust
    /// decoder spin-up cost); everything else (m4a / mp3 / aac / flac /
    /// ogg) goes through the Rust `dimmy_compute_audio_peaks` FFI
    /// which shares the Symphonia decoder used by `dimmy_transcribe_file`.
    ///
    /// Returns the same shape `ReadPeaks` always has (float[bucketCount]
    /// in [0..1]), or an empty array if both paths fail. The caller
    /// (History detail panel) treats "empty" as "no waveform" UI.
    public static float[] ReadPeaksAny(string path, int bucketCount)
    {
        if (string.IsNullOrEmpty(path) || bucketCount <= 0)
            return Array.Empty<float>();
        var ext = Path.GetExtension(path);
        if (!string.IsNullOrEmpty(ext)
            && ext.Equals(".wav", StringComparison.OrdinalIgnoreCase))
        {
            var wavPeaks = ReadPeaks(path, bucketCount);
            if (wavPeaks.Length > 0) return wavPeaks;
            // Fall through if the WAV magic was missing — symphonia
            // can recover from some odd containers (mp4 mis-named .wav)
            // where hound would just fail silently.
        }
        return ReadPeaksViaFfi(path, bucketCount);
    }

    /// Pure-FFI path used for non-WAV formats (m4a / mp3 / aac / flac /
    /// ogg). Allocates a JSON buffer sized for `bucketCount` 32-bit
    /// floats + framing overhead (~16 bytes per number including
    /// commas and brackets), with a small extra for the duration_secs
    /// field. Empty array on any error — host falls back to "no
    /// waveform".
    private static float[] ReadPeaksViaFfi(string path, int bucketCount)
    {
        try
        {
            // Heuristic: each peak serialises to at most ~14 chars
            // ("0.1234567,"), plus 64 bytes for the duration + brackets
            // + keys + null terminator. Round up to the next 4 KB
            // boundary for safety on very wide canvases.
            int needed = bucketCount * 16 + 128;
            int bufLen = ((needed + 4095) / 4096) * 4096;
            var buf = new byte[bufLen];
            int rc = Interop.DimmyNative.dimmy_compute_audio_peaks(
                path, bucketCount, buf, bufLen);
            if (rc <= 0) return Array.Empty<float>();
            string json = Encoding.UTF8.GetString(buf, 0, rc);
            using var doc = JsonDocument.Parse(json);
            if (!doc.RootElement.TryGetProperty("peaks", out var arr)
                || arr.ValueKind != JsonValueKind.Array)
                return Array.Empty<float>();
            int n = arr.GetArrayLength();
            var peaks = new float[n];
            int i = 0;
            foreach (var el in arr.EnumerateArray())
            {
                peaks[i++] = (float)el.GetDouble();
            }
            return peaks;
        }
        catch
        {
            return Array.Empty<float>();
        }
    }

    /// Stride-sampling tunables.
    /// At 200 buckets × 8 runs × 64 samples = 102 400 sample reads
    /// regardless of file size, vs the old "read every sample" loop
    /// which on a 4940 s / 48 kHz int16 mono file made 237 million
    /// ReadInt16() virtual calls. Peak fidelity on voice / music is
    /// indistinguishable to the eye — we'd only miss a single-sample
    /// transient (a click), which is invisible at 60 px tall anyway.
    private const int MaxStrideRunsPerBucket = 8;
    private const int RunSamplesPerStride = 64;

    /// Peaks-cache sidecar. Written next to `audio.wav` as
    /// `audio.wav.peaks.json`. Invalidation: audio file size changes
    /// (the only way our pipeline rewrites the data section).
    private sealed class PeaksCache
    {
        public long AudioSize { get; set; }
        public int Buckets { get; set; }
        public float[] Peaks { get; set; } = Array.Empty<float>();
    }

    public static float[] ReadPeaks(string path, int bucketCount)
    {
        if (string.IsNullOrEmpty(path) || !File.Exists(path)) return Array.Empty<float>();
        if (bucketCount <= 0) return Array.Empty<float>();

        // Cache hit path — ~1 ms for any file size. Invalidated by
        // audio-size mismatch (cheap stat) so a re-decoded WAV
        // produces a fresh cache automatically.
        var cached = TryReadCachedPeaks(path, bucketCount);
        if (cached.Length > 0) return cached;

        var peaks = ComputePeaks(path, bucketCount);
        if (peaks.Length > 0) TryWriteCachedPeaks(path, peaks);
        return peaks;
    }

    private static float[] ComputePeaks(string path, int bucketCount)
    {
        try
        {
            using var fs = File.OpenRead(path);
            using var br = new BinaryReader(fs);

            if (new string(br.ReadChars(4)) != "RIFF") return Array.Empty<float>();
            br.ReadUInt32(); // file size
            if (new string(br.ReadChars(4)) != "WAVE") return Array.Empty<float>();

            ushort fmtTag = 0;
            ushort channels = 1;
            uint sampleRate = 0;
            ushort bitsPerSample = 0;
            long dataOffset = -1;
            uint dataSize = 0;

            // Walk chunks until we see fmt + data.
            while (fs.Position + 8 <= fs.Length)
            {
                var id = new string(br.ReadChars(4));
                var size = br.ReadUInt32();
                if (id == "fmt ")
                {
                    fmtTag = br.ReadUInt16();
                    channels = br.ReadUInt16();
                    sampleRate = br.ReadUInt32();
                    br.ReadUInt32(); // byte rate
                    br.ReadUInt16(); // block align
                    bitsPerSample = br.ReadUInt16();
                    var consumed = 16u;
                    if (size > consumed) fs.Seek(size - consumed, SeekOrigin.Current);
                }
                else if (id == "data")
                {
                    dataOffset = fs.Position;
                    dataSize = size;
                    break;
                }
                else
                {
                    fs.Seek(size, SeekOrigin.Current);
                }
            }

            if (dataOffset < 0 || channels == 0 || bitsPerSample == 0)
                return Array.Empty<float>();

            int bytesPerSample = bitsPerSample / 8;
            int frameSize = bytesPerSample * channels;
            if (frameSize == 0) return Array.Empty<float>();
            long totalFrames = dataSize / (uint)frameSize;
            if (totalFrames <= 0) return Array.Empty<float>();

            var peaks = new float[bucketCount];
            long framesPerBucket = Math.Max(1, totalFrames / bucketCount);
            // One small reusable byte buffer for stride-run reads —
            // sized for the worst plausible frame width (4 ch * 4 bytes).
            var runBuf = new byte[RunSamplesPerStride * frameSize];

            for (int b = 0; b < bucketCount; b++)
            {
                long startFrame = (long)b * framesPerBucket;
                long endFrame = Math.Min(totalFrames, (long)(b + 1) * framesPerBucket);
                long bucketFrames = endFrame - startFrame;
                if (bucketFrames <= 0) break;

                // For tiny buckets (frames < RunSamplesPerStride),
                // there's no point striding — read the whole thing.
                int runFrames = (int)Math.Min(RunSamplesPerStride, bucketFrames);
                int strideRuns = (int)Math.Min(MaxStrideRunsPerBucket,
                    Math.Max(1, bucketFrames / runFrames));
                long strideJump = bucketFrames / strideRuns;

                float peak = 0f;
                for (int s = 0; s < strideRuns; s++)
                {
                    long stridePos = startFrame + (long)s * strideJump;
                    int runLen = (int)Math.Min(runFrames, endFrame - stridePos);
                    if (runLen <= 0) break;
                    int runBytes = runLen * frameSize;

                    fs.Seek(dataOffset + stridePos * frameSize, SeekOrigin.Begin);
                    int got = fs.Read(runBuf, 0, runBytes);
                    if (got < runBytes) break;

                    float runPeak = PeakOfRun(runBuf.AsSpan(0, got), fmtTag, bitsPerSample, channels);
                    if (runPeak > peak) peak = runPeak;
                }
                peaks[b] = peak > 1f ? 1f : peak;
            }
            return peaks;
        }
        catch
        {
            return Array.Empty<float>();
        }
    }

    /// Peak of a contiguous PCM byte run. Dedicated fast paths for the
    /// formats we see in practice (16-bit int, 32-bit float); falls
    /// back to the general byte-walker for 8-bit / 24-bit etc.
    private static float PeakOfRun(ReadOnlySpan<byte> bytes, ushort fmtTag, ushort bits, ushort channels)
    {
        float peak = 0f;
        if (fmtTag == 1 && bits == 16)
        {
            var shorts = MemoryMarshal.Cast<byte, short>(bytes);
            for (int i = 0; i < shorts.Length; i++)
            {
                int v = shorts[i];
                int a = v < 0 ? -v : v;
                if (a > peak * 32768f) peak = a / 32768f;
            }
            return peak;
        }
        if (fmtTag == 3 && bits == 32)
        {
            var floats = MemoryMarshal.Cast<byte, float>(bytes);
            for (int i = 0; i < floats.Length; i++)
            {
                float a = MathF.Abs(floats[i]);
                if (a > peak) peak = a;
            }
            return peak;
        }
        // Slow fallback via BinaryReader on a MemoryStream — covers
        // 8-bit, 24-bit and 32-bit int variants.
        using var ms = new MemoryStream(bytes.ToArray(), writable: false);
        using var lbr = new BinaryReader(ms);
        int bytesPerSample = bits / 8;
        int frameSize = bytesPerSample * channels;
        if (frameSize == 0) return 0f;
        int frames = bytes.Length / frameSize;
        for (int f = 0; f < frames; f++)
        {
            for (int c = 0; c < channels; c++)
            {
                float v = ReadSample(lbr, fmtTag, bits);
                float a = MathF.Abs(v);
                if (a > peak) peak = a;
            }
        }
        return peak;
    }

    private static string CachePathFor(string audioPath) => audioPath + ".peaks.json";

    private static float[] TryReadCachedPeaks(string audioPath, int bucketCount)
    {
        try
        {
            var cachePath = CachePathFor(audioPath);
            if (!File.Exists(cachePath)) return Array.Empty<float>();
            long audioSize = new FileInfo(audioPath).Length;
            var cache = JsonSerializer.Deserialize<PeaksCache>(File.ReadAllText(cachePath));
            if (cache == null) return Array.Empty<float>();
            if (cache.AudioSize != audioSize) return Array.Empty<float>();
            if (cache.Buckets != bucketCount) return Array.Empty<float>();
            return cache.Peaks ?? Array.Empty<float>();
        }
        catch
        {
            return Array.Empty<float>();
        }
    }

    private static void TryWriteCachedPeaks(string audioPath, float[] peaks)
    {
        try
        {
            var cachePath = CachePathFor(audioPath);
            long audioSize = new FileInfo(audioPath).Length;
            var cache = new PeaksCache
            {
                AudioSize = audioSize,
                Buckets = peaks.Length,
                Peaks = peaks,
            };
            var tmpPath = cachePath + ".tmp";
            File.WriteAllText(tmpPath, JsonSerializer.Serialize(cache));
            // Atomic rename — readers either see the old cache or
            // the new one, never a half-written file.
            if (File.Exists(cachePath)) File.Delete(cachePath);
            File.Move(tmpPath, cachePath);
        }
        catch
        {
            // Cache is best-effort — a failure here just means we'll
            // recompute next time.
        }
    }

    /// <summary>
    /// Return the audio duration in seconds, or 0 if the file is
    /// missing / unreadable / not a WAV. Walks the same header as
    /// <see cref="ReadPeaks"/> but stops after the format/data
    /// chunk metadata, so it's cheap (no per-sample work).
    /// </summary>
    public static double ReadDurationSecs(string path)
    {
        try
        {
            if (string.IsNullOrEmpty(path) || !File.Exists(path)) return 0;
            using var fs = File.OpenRead(path);
            using var br = new BinaryReader(fs);
            if (new string(br.ReadChars(4)) != "RIFF") return 0;
            br.ReadUInt32();
            if (new string(br.ReadChars(4)) != "WAVE") return 0;

            ushort channels = 1;
            uint sampleRate = 0;
            ushort bitsPerSample = 0;
            uint dataSize = 0;

            while (fs.Position + 8 <= fs.Length)
            {
                var id = new string(br.ReadChars(4));
                var size = br.ReadUInt32();
                if (id == "fmt ")
                {
                    br.ReadUInt16(); // tag
                    channels = br.ReadUInt16();
                    sampleRate = br.ReadUInt32();
                    br.ReadUInt32(); // byte rate
                    br.ReadUInt16(); // block align
                    bitsPerSample = br.ReadUInt16();
                    var consumed = 16u;
                    if (size > consumed) fs.Seek(size - consumed, SeekOrigin.Current);
                }
                else if (id == "data")
                {
                    dataSize = size;
                    break;
                }
                else
                {
                    fs.Seek(size, SeekOrigin.Current);
                }
            }

            if (sampleRate == 0 || bitsPerSample == 0 || channels == 0) return 0;
            int bytesPerSample = bitsPerSample / 8;
            int frameSize = bytesPerSample * channels;
            if (frameSize == 0) return 0;
            long totalFrames = dataSize / (uint)frameSize;
            return (double)totalFrames / sampleRate;
        }
        catch { return 0; }
    }

    private static float ReadSample(BinaryReader br, ushort fmtTag, ushort bits)
    {
        // 1=PCM int, 3=IEEE float
        if (fmtTag == 3 && bits == 32)
            return br.ReadSingle();
        if (bits == 16)
            return br.ReadInt16() / 32768f;
        if (bits == 8)
            return (br.ReadByte() - 128) / 128f;
        if (bits == 24)
        {
            int b0 = br.ReadByte();
            int b1 = br.ReadByte();
            int b2 = (sbyte)br.ReadByte();
            int v = (b2 << 16) | (b1 << 8) | b0;
            return v / 8388608f;
        }
        if (bits == 32 && fmtTag == 1)
            return br.ReadInt32() / 2147483648f;
        // Unknown — skip frame to avoid an infinite loop, caller treats peak as 0.
        br.ReadBytes(bits / 8);
        return 0f;
    }
}

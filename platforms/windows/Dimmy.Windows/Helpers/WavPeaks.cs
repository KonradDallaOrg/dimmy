using System;
using System.IO;
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

    public static float[] ReadPeaks(string path, int bucketCount)
    {
        try
        {
            if (string.IsNullOrEmpty(path) || !File.Exists(path))
                return Array.Empty<float>();
            if (bucketCount <= 0) return Array.Empty<float>();

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

            fs.Seek(dataOffset, SeekOrigin.Begin);
            for (int b = 0; b < bucketCount; b++)
            {
                float peak = 0f;
                long startFrame = b * framesPerBucket;
                long endFrame = Math.Min(totalFrames, (b + 1) * framesPerBucket);
                long framesToRead = endFrame - startFrame;
                if (framesToRead <= 0) break;

                fs.Seek(dataOffset + startFrame * frameSize, SeekOrigin.Begin);
                for (long f = 0; f < framesToRead; f++)
                {
                    float maxChannel = 0f;
                    for (int c = 0; c < channels; c++)
                    {
                        float s = ReadSample(br, fmtTag, bitsPerSample);
                        var a = Math.Abs(s);
                        if (a > maxChannel) maxChannel = a;
                    }
                    if (maxChannel > peak) peak = maxChannel;
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

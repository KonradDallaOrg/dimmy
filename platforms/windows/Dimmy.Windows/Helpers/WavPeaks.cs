using System;
using System.IO;

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

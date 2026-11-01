using System;

namespace Dimmy.Windows.Helpers;

/// <summary>Where a live waveform bar sits on the layer that scrolls. A bar keeps
/// this position for its whole life: the layer moves, the bars do not, and a new
/// sample only touches the rectangle of the oldest one.</summary>
public static class LiveWaveformLayout
{
    /// <summary>X of sample <paramref name="k"/> on the layer. It enters at the
    /// right edge plus the slack, measured from the time the layer was at 0.</summary>
    public static double BarX(long k, double sampleMs, double pitch, double width, double slack, double originMs)
    {
        if (sampleMs <= 0 || pitch <= 0) throw new ArgumentOutOfRangeException(nameof(sampleMs));
        return width + slack * pitch + (k * sampleMs - originMs) / sampleMs * pitch;
    }

    /// <summary>The layer's offset at <paramref name="elapsedMs"/>: the same speed
    /// the compositor animation runs at.</summary>
    public static double LayerX(double elapsedMs, double sampleMs, double pitch, double originMs)
    {
        if (sampleMs <= 0 || pitch <= 0) throw new ArgumentOutOfRangeException(nameof(sampleMs));
        return -(elapsedMs - originMs) / sampleMs * pitch;
    }

    /// <summary>The rectangle of sample <paramref name="k"/> in a ring of
    /// <paramref name="count"/>. Older samples can have a negative k after a restart.</summary>
    public static int RingIndex(long k, int count)
    {
        if (count <= 0) throw new ArgumentOutOfRangeException(nameof(count));
        return (int)(((k % count) + count) % count);
    }

    /// <summary>Bars across the width, the ones waiting past the right edge, and
    /// two more so the oldest is fully past the left edge when it is reused.</summary>
    public static int Capacity(double width, double pitch, double slack, int min, int max)
    {
        if (pitch <= 0) throw new ArgumentOutOfRangeException(nameof(pitch));
        return Math.Clamp((int)(width / pitch) + (int)slack + 2, min, max);
    }
}

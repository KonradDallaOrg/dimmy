using System.Text;
using Dimmy.Windows.Interop;
using Xunit;

namespace Dimmy.Windows.Tests;

/// <summary>
/// <c>dimmy_get_config_json</c> copies at most <c>bufLen - 1</c> bytes and says
/// nothing when it cuts. A cut snapshot fails to parse and every reader falls
/// back to its default. These pin that the config reader never returns a cut
/// string.
/// </summary>
public class ReadGrowingTests
{
    // Mimics the core's write_to_buf: copy what fits without cutting a UTF-8
    // character, NUL-terminate, return the count.
    private static System.Func<byte[], int, int> Core(string value) => (buf, len) =>
    {
        var bytes = Encoding.UTF8.GetBytes(value);
        int n = System.Math.Min(bytes.Length, len - 1);
        while (n < bytes.Length && n > 0 && (bytes[n] & 0xC0) == 0x80) n--;
        System.Array.Copy(bytes, buf, n);
        buf[n] = 0;
        return n;
    };

    [Fact]
    public void Small_snapshot_is_returned_whole()
    {
        Assert.Equal("{\"a\":1}", DimmyNative.ReadGrowing(Core("{\"a\":1}")));
    }

    [Fact]
    public void Snapshot_larger_than_the_first_buffer_is_returned_whole()
    {
        var big = "{\"p\":\"" + new string('x', 300_000) + "\"}";
        Assert.Equal(big, DimmyNative.ReadGrowing(Core(big)));
    }

    [Fact]
    public void Snapshot_exactly_filling_a_buffer_is_retried_not_returned_cut()
    {
        var exact = new string('y', (1 << 16) - 1);
        Assert.Equal(exact, DimmyNative.ReadGrowing(Core(exact)));
    }

    [Fact]
    public void Snapshot_cut_before_a_multibyte_character_is_retried_not_returned_cut()
    {
        // The core stops short of a character it cannot fit whole, so a cut
        // result can be up to 3 bytes below bufLen - 1.
        var cut = new string('z', (1 << 16) - 3) + "€" + "tail";
        Assert.Equal(cut, DimmyNative.ReadGrowing(Core(cut)));
    }

    [Fact]
    public void Core_error_returns_null()
    {
        Assert.Null(DimmyNative.ReadGrowing((_, _) => -1));
    }
}

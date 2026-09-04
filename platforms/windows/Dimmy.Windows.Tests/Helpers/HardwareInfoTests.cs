using Dimmy.Windows.Helpers;
using Xunit;

namespace Dimmy.Windows.Tests.Helpers;

public class HardwareInfoTests
{
    [Fact]
    public void Parses_what_the_core_actually_returns()
    {
        // Captured from dimmy_hardware_json on the machine this was written
        // on: a 4 GB card reports 3938 MB because it reserves a slice.
        var json = """
        {"name":"NVIDIA T600 Laptop GPU","vram_mb":3938,"dedicated":true,
         "apple_silicon":false,"fitness":"good",
         "line":"NVIDIA T600 Laptop GPU · 3.8 GB — transcription runs here"}
        """;
        var info = HardwareInfo.Parse(json);
        Assert.NotNull(info);
        Assert.Equal("NVIDIA T600 Laptop GPU", info!.Name);
        Assert.Equal(3938, info.VramMb);
        Assert.True(info.Dedicated);
        Assert.False(info.AppleSilicon);
        Assert.Equal("good", info.Fitness);
        Assert.Contains("transcription runs here", info.Line);
    }

    [Fact]
    public void A_failed_probe_parses_without_inventing_anything()
    {
        var json = """
        {"name":null,"vram_mb":null,"dedicated":false,"apple_silicon":false,
         "fitness":"unknown","line":null}
        """;
        var info = HardwareInfo.Parse(json);
        Assert.NotNull(info);
        Assert.Null(info!.Name);
        Assert.Null(info.VramMb);
        Assert.Null(info.Line);
        Assert.Equal("unknown", info.Fitness);
    }

    [Fact]
    public void Fitness_survives_a_payload_with_only_that_field()
    {
        // What onboarding actually consumes: the fitness string, handed
        // straight to OnboardingPreselect. The recommendation itself lives
        // there and is tested there — one answer to one question.
        var info = HardwareInfo.Parse(
            """{"fitness":"poor","dedicated":false,"line":"Intel UHD - Cloud is a better start"}""");
        Assert.NotNull(info);
        Assert.Equal("poor", info!.Fitness);
        Assert.Contains("Cloud is a better start", info.Line);
    }

    [Theory]
    [InlineData(null)]
    [InlineData("")]
    [InlineData("   ")]
    [InlineData("not json at all")]
    [InlineData("[1,2,3]")]
    [InlineData("\"a bare string\"")]
    public void Nothing_usable_yields_null_rather_than_throwing(string? json)
    {
        // A hardware hint is never worth breaking onboarding over.
        Assert.Null(HardwareInfo.Parse(json));
    }

    [Fact]
    public void A_truncated_payload_yields_null()
    {
        Assert.Null(HardwareInfo.Parse("""{"name":"NVIDIA","vram_mb":39"""));
    }
}

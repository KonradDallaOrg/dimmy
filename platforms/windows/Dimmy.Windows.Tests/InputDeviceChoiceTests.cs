using Dimmy.Windows.Helpers;
using Xunit;

namespace Dimmy.Windows.Tests;

public class InputDeviceChoiceTests
{
    private static readonly string[] Connected = { "Mic A", "Mic B" };

    [Theory]
    [InlineData(null)]
    [InlineData("")]
    public void NoSavedDeviceShowsSystemDefault(string? saved) =>
        Assert.Equal(InputDeviceChoice.SystemDefault, InputDeviceChoice.ForDisplay(saved));

    [Fact]
    public void SystemDefaultComesFirst() =>
        Assert.Equal(new[] { InputDeviceChoice.SystemDefault, "Mic A", "Mic B" },
            InputDeviceChoice.Entries(Connected, InputDeviceChoice.SystemDefault));

    [Fact]
    public void ASavedDeviceThatIsGoneStaysInTheList() =>
        Assert.Equal(new[] { InputDeviceChoice.SystemDefault, "Mic A", "Mic B", "Old mic" },
            InputDeviceChoice.Entries(Connected, "Old mic"));

    [Fact]
    public void OnlyTheSystemDefaultEntryClearsTheDevice()
    {
        Assert.Equal("", InputDeviceChoice.ConfigValue(InputDeviceChoice.SystemDefault));
        Assert.Equal("Mic A", InputDeviceChoice.ConfigValue("Mic A"));
        // A view model that has not loaded yet must not wipe the saved choice.
        Assert.Null(InputDeviceChoice.ConfigValue(null));
        Assert.Null(InputDeviceChoice.ConfigValue(""));
    }

    [Fact]
    public void ASavedDeviceThatIsGoneIsNotTheOneInUse()
    {
        Assert.Equal("Mic A", InputDeviceChoice.InUse("Mic A", Connected));
        Assert.Equal(InputDeviceChoice.SystemDefault, InputDeviceChoice.InUse("Old mic", Connected));
        Assert.Equal(InputDeviceChoice.SystemDefault, InputDeviceChoice.InUse(InputDeviceChoice.SystemDefault, Connected));
    }
}

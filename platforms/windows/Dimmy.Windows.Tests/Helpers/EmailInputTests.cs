using Dimmy.Windows.Helpers;
using Xunit;

namespace Dimmy.Windows.Tests.Helpers;

public class EmailInputTests
{
    [Theory]
    [InlineData("anna@example.com")]
    [InlineData("  anna.rossi+dimmy@mail.example.co.uk  ")]
    [InlineData("a@b.it")]
    public void AcceptsAddresses(string text) => Assert.True(EmailInput.LooksValid(text));

    [Theory]
    [InlineData(null)]
    [InlineData("")]
    [InlineData("anna")]
    [InlineData("anna@")]
    [InlineData("@example.com")]
    [InlineData("anna@example")]
    [InlineData("anna@example.")]
    [InlineData("anna@.com")]
    [InlineData("anna@exa mple.com")]
    [InlineData("anna@@example.com")]
    [InlineData("anna@example..com")]
    public void RejectsWhatIsNotAnAddress(string? text) => Assert.False(EmailInput.LooksValid(text));
}

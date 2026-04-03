using Dimmy.Windows.Services;
using Xunit;

namespace Dimmy.Windows.Tests.Services;

public class TranscriptionResultTests
{
    // ── Factory methods ──

    [Fact]
    public void Success_SetsTextAndIsSuccess()
    {
        var result = TranscriptionResult.Success("hello world");
        Assert.True(result.IsSuccess);
        Assert.False(result.IsEmpty);
        Assert.False(result.IsTimeout);
        Assert.Equal("hello world", result.Text);
        Assert.Null(result.Error);
    }

    [Fact]
    public void Empty_IsEmptyAndNotSuccess()
    {
        var result = TranscriptionResult.Empty();
        Assert.True(result.IsEmpty);
        Assert.False(result.IsSuccess);
        Assert.False(result.IsTimeout);
        Assert.Null(result.Text);
        Assert.Null(result.Error);
    }

    [Fact]
    public void Timeout_HasErrorAndIsTimeout()
    {
        var result = TranscriptionResult.Timeout("timed out");
        Assert.True(result.IsTimeout);
        Assert.False(result.IsSuccess);
        Assert.False(result.IsEmpty);
        Assert.Equal("timed out", result.Error);
        Assert.Null(result.Text);
    }

    // ── Mutual exclusivity of states ──

    [Fact]
    public void States_AreMutuallyExclusive()
    {
        // Success: only IsSuccess is true
        var success = TranscriptionResult.Success("text");
        Assert.True(success.IsSuccess && !success.IsEmpty && !success.IsTimeout);

        // Empty: only IsEmpty is true
        var empty = TranscriptionResult.Empty();
        Assert.True(!empty.IsSuccess && empty.IsEmpty && !empty.IsTimeout);

        // Timeout: only IsTimeout is true
        var timeout = TranscriptionResult.Timeout("err");
        Assert.True(!timeout.IsSuccess && !timeout.IsEmpty && timeout.IsTimeout);
    }

    // ── Edge cases ──

    [Fact]
    public void Success_EmptyString_IsStillSuccess()
    {
        // Empty string is NOT null, so IsSuccess is true
        // (the pipeline should prevent this, but the type should handle it)
        var result = TranscriptionResult.Success("");
        Assert.True(result.IsSuccess);
        Assert.Equal("", result.Text);
    }

    [Fact]
    public void Success_PreservesUnicode()
    {
        var result = TranscriptionResult.Success("Ciao, come stai? È bello! 你好");
        Assert.Equal("Ciao, come stai? È bello! 你好", result.Text);
    }

    [Fact]
    public void Success_PreservesLongText()
    {
        var longText = new string('A', 10000);
        var result = TranscriptionResult.Success(longText);
        Assert.Equal(10000, result.Text!.Length);
    }

    [Fact]
    public void Timeout_PreservesErrorMessage()
    {
        var result = TranscriptionResult.Timeout("Transcription timed out (30s)");
        Assert.Equal("Transcription timed out (30s)", result.Error);
    }
}

using System.Collections.Generic;
using Dimmy.Windows.Services;
using Dimmy.Windows.ViewModels;
using Xunit;

namespace Dimmy.Windows.Tests.Services;

public class ModelDownloadCenterTests
{
    [Fact]
    public void An_event_updates_the_job_and_notifies_once()
    {
        var center = new ModelDownloadCenter();
        var seen = new List<ModelDownloadJob>();
        center.Changed += seen.Add;

        center.Apply("{\"id\":\"ggml-small.bin\",\"state\":\"downloading\",\"done\":50,\"total\":200}");

        var job = Assert.Single(seen);
        Assert.Equal("ggml-small.bin", job.Id);
        Assert.True(job.IsActive);
        Assert.Equal(25.0, job.Percent);
        Assert.Same(job, center.Get("ggml-small.bin"));
    }

    [Fact]
    public void Unknown_size_has_no_percent()
    {
        var center = new ModelDownloadCenter();
        center.Apply("{\"id\":\"m.gguf\",\"state\":\"downloading\",\"done\":5,\"total\":0}");
        Assert.Null(center.Get("m.gguf")!.Percent);
    }

    [Fact]
    public void A_failure_keeps_its_error_and_is_not_active()
    {
        var center = new ModelDownloadCenter();
        center.Apply("{\"id\":\"m.gguf\",\"state\":\"failed\",\"done\":5,\"total\":10,\"error\":\"timeout\"}");
        var job = center.Get("m.gguf")!;
        Assert.False(job.IsActive);
        Assert.Equal("timeout", job.Error);
    }

    [Fact]
    public void The_snapshot_fills_every_job()
    {
        var center = new ModelDownloadCenter();
        center.LoadSnapshot(
            "[{\"id\":\"a.bin\",\"state\":\"done\",\"done\":1,\"total\":1}," +
            "{\"id\":\"parakeet:fp32\",\"state\":\"queued\",\"done\":0,\"total\":0}]");
        Assert.Equal("done", center.Get("a.bin")!.State);
        Assert.True(center.Get("parakeet:fp32")!.IsActive);
        Assert.Null(center.Get("never"));
    }

    [Fact]
    public void Garbage_is_ignored()
    {
        var center = new ModelDownloadCenter();
        center.Apply("not json");
        center.Apply("{\"state\":\"done\"}");
        center.LoadSnapshot(null);
        center.LoadSnapshot("{}");
        Assert.Null(center.Get(""));
    }

    [Fact]
    public void The_core_event_reaches_the_shared_center()
    {
        var vm = new AppViewModel();
        vm.HandleEvent(
            "{\"event\":\"model_download\",\"payload\":{\"id\":\"vm-test.bin\",\"state\":\"done\",\"done\":3,\"total\":3}}");
        Assert.Equal("done", ModelDownloadCenter.Instance.Get("vm-test.bin")!.State);
    }
}

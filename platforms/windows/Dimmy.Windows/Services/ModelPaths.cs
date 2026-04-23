using System;
using System.IO;

namespace Dimmy.Windows.Services;

// Mirrors core/src/local_stt.rs constants. Single point of update if Rust changes
// MODEL_BASE_URL or model_directory() — everything else in onboarding reads from here.
public static class ModelPaths
{
    public const string HfBaseUrl = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";

    public const string BaseModelFilename = "ggml-base-q8_0.bin";
    public const long BaseModelSizeBytes = 78L * 1024 * 1024;

    public const string SmallModelFilename = "ggml-small-q5_1.bin";
    public const long SmallModelSizeBytes = 181L * 1024 * 1024;

    public static string ModelsDirectory => Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData),
        "dimmy",
        "models");

    public static string GetModelFilePath(string filename) =>
        Path.Combine(ModelsDirectory, filename);

    public static string GetPartFilePath(string filename) =>
        Path.Combine(ModelsDirectory, filename + ".part");

    public static string GetDownloadUrl(string filename) =>
        $"{HfBaseUrl}/{filename}";
}

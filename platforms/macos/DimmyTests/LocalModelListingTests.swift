import XCTest
@testable import Dimmy

/// The local-model list is the only place the user learns which engine will
/// run and where. Two things there are load-bearing and silent when wrong:
/// the accelerator suffix on each row, and the name the menu bar and the
/// Home page show for the running backend.
final class LocalModelListingTests: XCTestCase {

    private func model(_ filename: String, _ name: String, _ mb: Int) -> [String: Any] {
        ["filename": filename, "name": name, "size_mb": mb]
    }

    // MARK: - Accelerator suffix

    func testWhisperRowSaysNeuralEngineOnlyWhenTheEncoderIsOnDisk() {
        let m = model("ggml-large-v3-turbo-q8_0.bin", "Large-v3-Turbo Q8", 874)
        XCTAssertEqual(
            MacVoicePage.whisperRowLabel(m, neuralEngine: true),
            "Large-v3-Turbo Q8 · 874 MB · Neural Engine"
        )
        // Promising the Neural Engine for a model whose encoder was never
        // downloaded is worse than saying nothing: it runs on the GPU.
        XCTAssertEqual(
            MacVoicePage.whisperRowLabel(m, neuralEngine: false),
            "Large-v3-Turbo Q8 · 874 MB · GPU"
        )
    }

    func testQwenNeuralEngineVariantsDoNotRepeatTheAccelerator() {
        // Their catalog name already ends in "· Neural Engine (int8)".
        let ne = model("fluid:qwen3-asr-0.6b-int8", "Qwen3-ASR 0.6B · Neural Engine (int8)", 2865)
        XCTAssertFalse(MacVoicePage.qwenRowLabel(ne).hasSuffix("· GPU"))
        let gguf = model("Qwen3-ASR-1.7B-Q8_0.gguf", "Qwen3-ASR 1.7B", 2404)
        XCTAssertTrue(MacVoicePage.qwenRowLabel(gguf).hasSuffix("· GPU"))
    }

    func testSizesOverAGigabyteReadAsGigabytes() {
        let m = model("ggml-large-v3-q5_0.bin", "Large-v3 Q5", 1104)
        XCTAssertEqual(MacVoicePage.modelLabel(m), "Large-v3 Q5 · 1.1 GB")
    }

    // MARK: - What the pill and the Home page call the running backend

    // AppState is @MainActor isolated, so its statics are too.
    @MainActor
    func testDisplayNameNamesTheEngineNotTheCloudProvider() {
        XCTAssertEqual(
            AppState.localSttDisplayName(backend: "parakeet", qwenModel: "", whisperModel: ""),
            "Parakeet"
        )
        XCTAssertEqual(
            AppState.localSttDisplayName(
                backend: "qwen",
                qwenModel: "Qwen3-ASR-1.7B-Q8_0.gguf",
                whisperModel: ""
            ),
            "Qwen3-ASR"
        )
        XCTAssertEqual(
            AppState.localSttDisplayName(
                backend: "qwen",
                qwenModel: "fluid:qwen3-asr-0.6b-int8",
                whisperModel: ""
            ),
            "Qwen3-ASR · Neural Engine"
        )
        XCTAssertEqual(
            AppState.localSttDisplayName(
                backend: "whisper",
                qwenModel: "",
                whisperModel: "ggml-large-v3-turbo-q8_0.bin"
            ),
            "Whisper large-v3-turbo-q8_0"
        )
    }
}

/// The recap picker is the one model list that never said whether the model
/// was on disk: the Voice and LLM pickers both mark downloaded entries, so a
/// user reasonably reads the absence of a mark as "nothing to download".
final class RecapLocalModelTests: XCTestCase {

    func testALocalOptionNamesTheFileItWouldRun() {
        let opt = RecapModelOption(
            id: "local:gemma-4-E2B-it-qat-UD-Q4_K_XL.gguf",
            label: "Local, Gemma 4 E2B QAT Q4",
            provider: .local
        )
        XCTAssertEqual(opt.localFilename, "gemma-4-E2B-it-qat-UD-Q4_K_XL.gguf")
    }

    func testCloudAndAutoOptionsHaveNoFile() {
        XCTAssertNil(
            RecapModelOption(id: "", label: "Auto", provider: .auto).localFilename
        )
        XCTAssertNil(
            RecapModelOption(id: "claude-sonnet-5", label: "Sonnet", provider: .anthropic)
                .localFilename
        )
        // A bare prefix names nothing, and must not be probed as a filename.
        XCTAssertNil(
            RecapModelOption(id: "local:", label: "Local", provider: .local).localFilename
        )
    }

    @MainActor
    func testEveryCuratedLocalOptionCarriesARealFilename() {
        for opt in RecapModelOption.curated where opt.provider == .local {
            XCTAssertNotNil(opt.localFilename, "\(opt.id) has no file to check")
            XCTAssertTrue(opt.localFilename?.hasSuffix(".gguf") == true, "\(opt.id)")
        }
    }
}

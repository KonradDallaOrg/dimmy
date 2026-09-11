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

    @MainActor
    func testDisplayNameNamesTheEngineNotTheCloudProvider() {
        let s = AppState()
        s.localSttBackend = "parakeet"
        XCTAssertEqual(s.localSttDisplayName, "Parakeet")

        s.localSttBackend = "qwen"
        s.qwenAsrModel = "Qwen3-ASR-1.7B-Q8_0.gguf"
        XCTAssertEqual(s.localSttDisplayName, "Qwen3-ASR")
        s.qwenAsrModel = "fluid:qwen3-asr-0.6b-int8"
        XCTAssertEqual(s.localSttDisplayName, "Qwen3-ASR · Neural Engine")

        s.localSttBackend = "whisper"
        s.localModel = "ggml-large-v3-turbo-q8_0.bin"
        XCTAssertEqual(s.localSttDisplayName, "Whisper large-v3-turbo-q8_0")
    }
}

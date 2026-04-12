import Foundation

// MARK: - DimmyCore — Swift wrapper around the Rust FFI (ffi.rs)

/// Singleton that owns the Rust core lifecycle.
/// All methods are thread-safe (Rust uses Mutex internally).
final class DimmyCore {
    static let shared = DimmyCore()

    /// Standard buffer size for FFI string returns (64 KB).
    private static let bufferSize: Int32 = 65_536

    /// Large buffer for transcripts (512 KB).
    private static let transcriptBufferSize: Int32 = 524_288

    private init() {}

    // MARK: - Lifecycle

    /// Initialize the Rust core. Call once at app launch.
    /// Returns true on success.
    @discardableResult
    func initialize() -> Bool {
        let result = dimmy_init()
        if result == 0 {
            registerEventCallback()
            print("[DimmyCore] initialized successfully")
        } else {
            print("[DimmyCore] ERROR: init failed with code \(result)")
        }
        return result == 0
    }

    /// Shut down the Rust core. Call on app termination.
    func shutdown() {
        dimmy_shutdown()
        print("[DimmyCore] shutdown")
    }

    // MARK: - Event Callback

    private func registerEventCallback() {
        dimmy_set_event_callback(dimmyEventHandler)
    }

    // MARK: - Recording

    /// Start recording. Returns 0=OK, -1=no API key, -2=already recording.
    func startRecording() -> Int32 {
        let result = dimmy_start_recording()
        print("[DimmyCore] startRecording → \(result)")
        return result
    }

    /// Stop recording and return transcript. Blocking — call from background thread.
    func stopRecording() -> String? {
        let bufLen = Self.transcriptBufferSize
        let buffer = UnsafeMutablePointer<CChar>.allocate(capacity: Int(bufLen))
        defer { buffer.deallocate() }
        buffer[0] = 0

        let written = dimmy_stop_recording(buffer, bufLen)
        if written < 0 {
            print("[DimmyCore] ERROR: stopRecording failed with code \(written)")
            return nil
        }
        if written == 0 {
            return ""
        }
        return String(cString: buffer)
    }

    /// Cancel recording without transcribing.
    func cancelRecording() {
        dimmy_cancel_recording()
        print("[DimmyCore] recording cancelled")
    }

    // MARK: - Config

    /// Get full config as parsed JSON dictionary.
    func getConfig() -> [String: Any]? {
        let bufLen = Self.bufferSize
        let buffer = UnsafeMutablePointer<CChar>.allocate(capacity: Int(bufLen))
        defer { buffer.deallocate() }
        buffer[0] = 0

        let written = dimmy_get_config_json(buffer, bufLen)
        guard written > 0 else { return nil }

        let jsonStr = String(cString: buffer)
        guard let data = jsonStr.data(using: .utf8),
              let dict = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        else { return nil }

        return dict
    }

    /// Set config from a dictionary. Returns true on success.
    @discardableResult
    func setConfig(_ config: [String: Any]) -> Bool {
        guard let data = try? JSONSerialization.data(withJSONObject: config),
              let jsonStr = String(data: data, encoding: .utf8)
        else { return false }

        return jsonStr.withCString { ptr in
            dimmy_set_config_json(ptr) == 0
        }
    }

    // MARK: - Audio

    /// Get current microphone amplitude (0.0 - 1.0).
    func getAmplitude() -> Float {
        dimmy_get_amplitude()
    }

    /// Get list of audio input device names.
    func listDevices() -> [String] {
        let bufLen = Self.bufferSize
        let buffer = UnsafeMutablePointer<CChar>.allocate(capacity: Int(bufLen))
        defer { buffer.deallocate() }
        buffer[0] = 0

        let written = dimmy_list_devices_json(buffer, bufLen)
        guard written > 0 else { return [] }

        let jsonStr = String(cString: buffer)
        guard let data = jsonStr.data(using: .utf8),
              let arr = try? JSONSerialization.jsonObject(with: data) as? [String]
        else { return [] }

        return arr
    }

    /// Check audio device health. Returns diagnostic JSON.
    func checkAudioHealth() -> [String: Any]? {
        let bufLen = Self.bufferSize
        let buffer = UnsafeMutablePointer<CChar>.allocate(capacity: Int(bufLen))
        defer { buffer.deallocate() }
        buffer[0] = 0

        let written = dimmy_check_audio_health(buffer, bufLen)
        guard written > 0 else { return nil }

        let jsonStr = String(cString: buffer)
        guard let data = jsonStr.data(using: .utf8),
              let dict = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        else { return nil }

        return dict
    }

    // MARK: - LLM

    /// Cycle LLM style forward (+1) or backward (-1).
    func cycleLlmStyle(direction: Int32) {
        dimmy_cycle_llm_style(direction)
    }

    /// Cycle LLM tone forward (+1) or backward (-1).
    func cycleLlmTone(direction: Int32) {
        dimmy_cycle_llm_tone(direction)
    }

    /// Process text through LLM enhancement. Blocking — call from background thread.
    /// Returns enhanced text, or original text on failure.
    func processWithLLM(text: String) -> String {
        let bufLen = Self.transcriptBufferSize
        let buffer = UnsafeMutablePointer<CChar>.allocate(capacity: Int(bufLen))
        defer { buffer.deallocate() }
        buffer[0] = 0

        let written = text.withCString { textPtr in
            dimmy_process_with_llm(textPtr, buffer, bufLen)
        }

        if written < 0 {
            print("[DimmyCore] ERROR: processWithLLM failed")
            return text
        }
        if written == 0 {
            return text
        }
        return String(cString: buffer)
    }

    // MARK: - Stats

    /// Update cumulative stats.
    @discardableResult
    func updateStats(words: Int32, speakingSecs: Double) -> Bool {
        dimmy_update_stats(words, speakingSecs) == 0
    }

    // MARK: - Utility

    /// Check if an API key is configured.
    var hasApiKey: Bool {
        dimmy_has_api_key() == 1
    }

    /// Check if recording is active.
    var isRecording: Bool {
        dimmy_is_recording() == 1
    }

    // MARK: - Local STT Models

    /// Get available local models with download status. Returns JSON array of dicts.
    func listLocalModels() -> [[String: Any]]? {
        let bufLen = Self.bufferSize
        let buffer = UnsafeMutablePointer<CChar>.allocate(capacity: Int(bufLen))
        defer { buffer.deallocate() }
        buffer[0] = 0

        let written = dimmy_list_local_models(buffer, bufLen)
        guard written > 0 else { return nil }

        let jsonStr = String(cString: buffer)
        guard let data = jsonStr.data(using: .utf8),
              let arr = try? JSONSerialization.jsonObject(with: data) as? [[String: Any]]
        else { return nil }

        return arr
    }

    /// Download a model file. BLOCKING — call from a background thread.
    /// Returns true on success.
    func downloadModel(_ filename: String) -> Bool {
        let result = filename.withCString { ptr in
            dimmy_download_model(ptr)
        }
        if result != 0 {
            print("[DimmyCore] ERROR: downloadModel(\(filename)) failed with code \(result)")
        }
        return result == 0
    }

    /// Check if a model file exists locally.
    func modelExists(_ filename: String) -> Bool {
        filename.withCString { ptr in
            dimmy_model_exists(ptr) == 1
        }
    }

    // MARK: - Local LLM Models

    /// List available local LLM models with download status.
    func listLLMModels() -> [[String: Any]]? {
        let bufLen = Self.bufferSize
        let buffer = UnsafeMutablePointer<CChar>.allocate(capacity: Int(bufLen))
        defer { buffer.deallocate() }
        buffer[0] = 0

        let written = dimmy_list_llm_models(buffer, bufLen)
        guard written > 0 else { return nil }

        let jsonStr = String(cString: buffer)
        guard let data = jsonStr.data(using: .utf8),
              let arr = try? JSONSerialization.jsonObject(with: data) as? [[String: Any]]
        else { return nil }

        return arr
    }

    /// Download an LLM model. Blocking — call from background thread.
    func downloadLLMModel(_ filename: String) -> Bool {
        let result = filename.withCString { ptr in
            dimmy_download_llm_model(ptr)
        }
        if result != 0 {
            print("[DimmyCore] ERROR: downloadLLMModel(\(filename)) failed with code \(result)")
        }
        return result == 0
    }

    /// Check if an LLM model file exists locally.
    func llmModelExists(_ filename: String) -> Bool {
        filename.withCString { ptr in
            dimmy_llm_model_exists(ptr) == 1
        }
    }

    // MARK: - Transcription History

    /// Save a transcript to history. Returns the transcript ID, or -1 on error.
    @discardableResult
    func historySave(text: String, language: String, duration: Double) -> Int32 {
        text.withCString { textPtr in
            language.withCString { langPtr in
                dimmy_history_save(textPtr, langPtr, duration)
            }
        }
    }

    /// Get recent transcripts as JSON array.
    func historyRecent(limit: Int32) -> [[String: Any]]? {
        let bufLen = Self.bufferSize
        let buffer = UnsafeMutablePointer<CChar>.allocate(capacity: Int(bufLen))
        defer { buffer.deallocate() }
        buffer[0] = 0

        let written = dimmy_history_recent(limit, buffer, bufLen)
        guard written > 0 else { return nil }

        let jsonStr = String(cString: buffer)
        guard let data = jsonStr.data(using: .utf8),
              let arr = try? JSONSerialization.jsonObject(with: data) as? [[String: Any]]
        else { return nil }

        return arr
    }

    /// Search transcripts via full-text search. Returns JSON array.
    func historySearch(query: String, limit: Int32) -> [[String: Any]]? {
        let bufLen = Self.bufferSize
        let buffer = UnsafeMutablePointer<CChar>.allocate(capacity: Int(bufLen))
        defer { buffer.deallocate() }
        buffer[0] = 0

        let written = query.withCString { qPtr in
            dimmy_history_search(qPtr, limit, buffer, bufLen)
        }
        guard written > 0 else { return nil }

        let jsonStr = String(cString: buffer)
        guard let data = jsonStr.data(using: .utf8),
              let arr = try? JSONSerialization.jsonObject(with: data) as? [[String: Any]]
        else { return nil }

        return arr
    }

    /// Delete a transcript by ID. Returns true on success.
    @discardableResult
    func historyDelete(id: Int32) -> Bool {
        dimmy_history_delete(id) == 0
    }

    /// Get history stats as JSON dictionary.
    func historyStats() -> [String: Any]? {
        let bufLen = Self.bufferSize
        let buffer = UnsafeMutablePointer<CChar>.allocate(capacity: Int(bufLen))
        defer { buffer.deallocate() }
        buffer[0] = 0

        let written = dimmy_history_stats(buffer, bufLen)
        guard written > 0 else { return nil }

        let jsonStr = String(cString: buffer)
        guard let data = jsonStr.data(using: .utf8),
              let dict = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        else { return nil }

        return dict
    }
}

// MARK: - Event Callback (C function, called from Rust)

/// Global C callback function registered with Rust. Receives JSON event strings.
/// Dispatches to AppState on the main thread.
private func dimmyEventHandler(_ jsonPtr: UnsafePointer<CChar>) {
    let jsonStr = String(cString: jsonPtr)

    guard let data = jsonStr.data(using: .utf8),
          let dict = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
          let event = dict["event"] as? String
    else {
        print("[DimmyCore] WARNING: received unparseable event: \(jsonStr.prefix(200))")
        return
    }

    let payload = dict["payload"] as? [String: Any] ?? [:]

    DispatchQueue.main.async {
        let appState = AppState.shared
        handleEvent(event: event, payload: payload, appState: appState)
    }
}

/// Handle a single event from Rust and update AppState accordingly.
@MainActor
private func handleEvent(event: String, payload: [String: Any], appState: AppState) {
    switch event {
    case "recording_started":
        // Recording state is set by HotkeyManager before calling startRecording
        break

    case "status":
        if let state = payload["state"] as? String {
            switch state {
            case "transcribing":
                appState.recordingState = .transcribing
            case "processing":
                appState.recordingState = .processing
            default:
                break
            }
        }

    case "transcript_ready":
        if let text = payload["text"] as? String {
            appState.lastTranscript = text
        }

    case "style_changed":
        if let style = payload["style"] as? String {
            appState.llmStyle = style
        }

    case "tone_changed":
        if let tone = payload["tone"] as? String {
            appState.llmTone = tone
        }

    case "chunk_progress":
        if let current = payload["current"] as? Int,
           let total = payload["total"] as? Int {
            appState.chunkProgress = (current, total)
        }

    case "error":
        if let message = payload["message"] as? String {
            appState.lastError = message
            print("[DimmyCore] error event: \(message)")
        }

    case "recording_cancelled":
        appState.recordingState = .idle

    case "model_download_progress":
        if let downloaded = payload["downloaded"] as? Int,
           let total = payload["total"] as? Int,
           total > 0 {
            appState.modelDownloadProgress = Double(downloaded) / Double(total)
        }

    case "llm_model_download_progress":
        if let downloaded = payload["downloaded"] as? Int,
           let total = payload["total"] as? Int,
           total > 0 {
            appState.llmModelDownloadProgress = Double(downloaded) / Double(total)
        }

    default:
        print("[DimmyCore] unhandled event: \(event)")
    }
}

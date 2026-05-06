import Foundation

// MARK: - DimmyCore — v2 features (app rules, history v2, file load, meeting, raw LLM)
//
// The v2 surface adds 4 user-visible features:
//   1. App rules — foreground-app capture at hotkey-down + per-rule overrides
//   2. History v2 — enhanced_text, audio_path, app_bundle_id, retention
//   3. Audio file load — drag-drop / NSOpenPanel → dimmy_transcribe_file
//   4. Meeting mode — long-form recording with streamed WAV + LLM recap
//
// All wrappers follow the existing convention: cstr-bridged calls in/out
// of Rust, JSON parsing for structured returns, log on error and return
// a Swift-friendly value (Optional / Result / Bool).

extension DimmyCore {
    // MARK: - App context

    /// Push the foreground-app snapshot to Rust before recording starts.
    /// Mac populates only `bundle_id` — the other two stay empty.
    /// Best-effort: any failure (no foreground app, etc.) returns silently.
    func setAppContext(bundleId: String) {
        guard isInitialized else { return }
        let trimmed = bundleId.trimmingCharacters(in: .whitespacesAndNewlines)
        let escaped = trimmed
            .replacingOccurrences(of: "\\", with: "\\\\")
            .replacingOccurrences(of: "\"", with: "\\\"")
        let json = "{\"process_name\":\"\",\"bundle_id\":\"\(escaped)\",\"wm_class\":\"\"}"
        _ = json.withCString { dimmy_set_app_context($0) }
    }

    /// Clear the foreground-app snapshot. Called after the LLM step
    /// resolves rules so a stale snapshot can't bleed into the next
    /// recording.
    func clearAppContext() {
        guard isInitialized else { return }
        dimmy_clear_app_context()
    }

    // MARK: - Audio file load

    /// Synchronously transcribe a WAV file via the active local STT
    /// backend. Blocking — call from a background thread. Progress
    /// events arrive on the main queue as `file_transcribe_progress`
    /// payloads (handled in DimmyCore.swift).
    ///
    /// Return codes:
    ///   - .success(text) — transcript (possibly empty if VAD removed all)
    ///   - .failure(.invalidArgs) — null pointer / bad UTF-8
    ///   - .failure(.openFailed) — couldn't decode WAV
    ///   - .failure(.silentInput) — preprocess removed all audio
    ///   - .failure(.cloudUnsupported) — sttMode is "cloud"
    ///   - .failure(.backendFailed) — local backend rejected the input
    func transcribeFile(at path: String) -> Result<String, FileTranscribeError> {
        guard isInitialized else { return .failure(.notInitialized) }
        let bufLen: Int32 = 1 << 20  // 1 MB transcript buffer
        let buffer = UnsafeMutablePointer<CChar>.allocate(capacity: Int(bufLen))
        defer { buffer.deallocate() }
        buffer[0] = 0
        let rc = path.withCString { p in dimmy_transcribe_file(p, buffer, bufLen) }
        if rc < 0 {
            switch rc {
            case -1: return .failure(.invalidArgs)
            case -2: return .failure(.openFailed)
            case -3: return .failure(.silentInput)
            case -4: return .failure(.cloudUnsupported)
            case -5: return .failure(.backendFailed)
            default: return .failure(.unknown(Int(rc)))
            }
        }
        if rc == 0 { return .success("") }
        return .success(String(cString: buffer))
    }

    enum FileTranscribeError: Error, CustomStringConvertible {
        case notInitialized
        case invalidArgs
        case openFailed
        case silentInput
        case cloudUnsupported
        case backendFailed
        case unknown(Int)

        var description: String {
            switch self {
            case .notInitialized: return "Dimmy core not initialized yet"
            case .invalidArgs: return "Invalid arguments"
            case .openFailed: return "Could not open or decode the WAV file"
            case .silentInput: return "Preprocess removed all audio (silent input?)"
            case .cloudUnsupported: return "Cloud STT is not supported via file load — use local mode"
            case .backendFailed: return "Local STT backend failed to transcribe"
            case .unknown(let code): return "transcribe_file failed (code \(code))"
            }
        }
    }

    // MARK: - Meeting mode

    /// Start a meeting recording. Returns the session id (UUID), or nil
    /// on failure (already active, audio start failure, etc.).
    func meetingStart() -> String? {
        guard isInitialized else { return nil }
        let bufLen: Int32 = 256
        let buffer = UnsafeMutablePointer<CChar>.allocate(capacity: Int(bufLen))
        defer { buffer.deallocate() }
        buffer[0] = 0
        let rc = dimmy_meeting_start(buffer, bufLen)
        guard rc > 0 else {
            print("[DimmyCore] meeting_start failed rc=\(rc)")
            return nil
        }
        return String(cString: buffer)
    }

    /// Stop the active meeting. Returns the parsed JSON dictionary, or
    /// nil if no meeting is active. Blocking up to ~1s — call from a
    /// background thread.
    func meetingStop() -> MeetingResult? {
        guard isInitialized else { return nil }
        let bufLen: Int32 = 1 << 22  // 4 MB transcript buffer
        let buffer = UnsafeMutablePointer<CChar>.allocate(capacity: Int(bufLen))
        defer { buffer.deallocate() }
        buffer[0] = 0
        let rc = dimmy_meeting_stop(buffer, bufLen)
        guard rc > 0 else {
            print("[DimmyCore] meeting_stop failed rc=\(rc)")
            return nil
        }
        let json = String(cString: buffer)
        guard let data = json.data(using: .utf8),
              let dict = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        else {
            print("[DimmyCore] meeting_stop: invalid JSON")
            return nil
        }
        return MeetingResult(dict: dict)
    }

    /// Persist the post-process LLM recap + actions into the meeting dir.
    /// Pass empty/nil for fields that shouldn't be written. 0 = ok.
    @discardableResult
    func meetingSavePostProcess(dir: String,
                                 recap: String?,
                                 actions: String?,
                                 translated: String? = nil) -> Bool {
        guard isInitialized else { return false }
        let result = dir.withCString { dirPtr -> Int32 in
            withOptionalCString(recap) { recapPtr in
                withOptionalCString(actions) { actionsPtr in
                    withOptionalCString(translated) { translatedPtr in
                        dimmy_meeting_save_post_process(dirPtr, recapPtr, actionsPtr, translatedPtr)
                    }
                }
            }
        }
        return result == 0
    }

    /// True while a meeting recording is active. Used to gate the
    /// dictation hotkey — parallel cpal recording corrupts both buffers.
    var meetingIsActive: Bool {
        dimmy_meeting_is_active() == 1
    }

    /// JSON array of crashed-meeting directories (`.recording` marker
    /// still present). UI surfaces this as a "recover meeting?" prompt.
    func meetingListOrphans() -> [[String: Any]] {
        guard isInitialized else { return [] }
        let bufLen: Int32 = 64 * 1024
        let buffer = UnsafeMutablePointer<CChar>.allocate(capacity: Int(bufLen))
        defer { buffer.deallocate() }
        buffer[0] = 0
        let rc = dimmy_meeting_list_orphans(buffer, bufLen)
        guard rc > 0 else { return [] }
        let json = String(cString: buffer)
        guard let data = json.data(using: .utf8),
              let arr = try? JSONSerialization.jsonObject(with: data) as? [[String: Any]]
        else { return [] }
        return arr
    }

    // MARK: - Raw LLM call

    /// Bypass the dictation rewrite wrapper and send a raw prompt to the
    /// configured LLM endpoint. Used for meeting recap + auto-recap.
    /// Blocking — call from a background thread.
    ///
    /// `modelOverride` empty = use the user's configured `llm_api_model`.
    /// Pass `claude-opus-4-7` for Anthropic recap, `gemini-2.5-pro` for
    /// Gemini recap, or "" to honour user pick.
    func llmCallRaw(prompt: String,
                     modelOverride: String = "",
                     maxTokens: Int32 = 4096) -> Result<String, LlmRawError> {
        guard isInitialized else { return .failure(.notInitialized) }
        guard !prompt.isEmpty else { return .failure(.emptyPrompt) }
        let bufLen: Int32 = 1 << 20
        let buffer = UnsafeMutablePointer<CChar>.allocate(capacity: Int(bufLen))
        defer { buffer.deallocate() }
        buffer[0] = 0
        let rc = prompt.withCString { promptPtr -> Int32 in
            modelOverride.withCString { modelPtr in
                dimmy_llm_call_raw(promptPtr, modelPtr, maxTokens, buffer, bufLen)
            }
        }
        if rc < 0 {
            switch rc {
            case -1: return .failure(.invalidArgs)
            case -2: return .failure(.notConfigured)
            case -3: return .failure(.httpError)
            default: return .failure(.unknown(Int(rc)))
            }
        }
        return .success(String(cString: buffer))
    }

    enum LlmRawError: Error, CustomStringConvertible {
        case notInitialized
        case emptyPrompt
        case invalidArgs
        case notConfigured  // missing api key/url
        case httpError
        case unknown(Int)

        var description: String {
            switch self {
            case .notInitialized: return "Core not initialized"
            case .emptyPrompt: return "Empty prompt"
            case .invalidArgs: return "Invalid arguments"
            case .notConfigured: return "LLM API URL or key is not configured"
            case .httpError: return "LLM HTTP / parse error — see dimmy.log"
            case .unknown(let code): return "llm_call_raw failed (code \(code))"
            }
        }
    }

    // MARK: - History v2 update hooks

    /// Backfill the enhanced_text column on a row created via historySave.
    /// Empty string clears. Called after the LLM rewrite returns so the
    /// detail UI can toggle Raw / Enhanced.
    @discardableResult
    func historyUpdateEnhanced(id: Int32, text: String) -> Bool {
        guard isInitialized else { return false }
        return text.withCString { ptr in
            dimmy_history_update_enhanced(id, ptr) == 0
        }
    }

    /// Set audio_path + size_bytes columns. Pass empty path to unlink.
    @discardableResult
    func historyUpdateAudio(id: Int32, path: String, sizeBytes: Int64) -> Bool {
        guard isInitialized else { return false }
        return path.withCString { ptr in
            dimmy_history_update_audio(id, ptr, sizeBytes) == 0
        }
    }

    /// Set the JSON-encoded word_timestamps column. Schema documented in
    /// core/src/history.rs. Empty clears.
    @discardableResult
    func historyUpdateWordTimestamps(id: Int32, json: String) -> Bool {
        guard isInitialized else { return false }
        return json.withCString { ptr in
            dimmy_history_update_word_timestamps(id, ptr) == 0
        }
    }
}

// MARK: - MeetingResult

/// Parsed result of `dimmy_meeting_stop`.
struct MeetingResult {
    let id: String
    let dir: String
    let transcript: String
    let durationSecs: Double
    let chunkCount: Int
    let error: String?

    init(dict: [String: Any]) {
        self.id = dict["id"] as? String ?? ""
        self.dir = dict["dir"] as? String ?? ""
        self.transcript = dict["transcript"] as? String ?? ""
        self.durationSecs = dict["duration_secs"] as? Double ?? 0.0
        self.chunkCount = dict["chunk_count"] as? Int ?? 0
        // `error` is null when the meeting completed cleanly.
        if let err = dict["error"] as? String, !err.isEmpty {
            self.error = err
        } else {
            self.error = nil
        }
    }
}

// MARK: - Helpers

/// Bridge an Optional<String> to a `const char *` (or null) for FFI.
/// `nil` and empty pass null so the Rust side hits its "skip this field"
/// branch — important for meeting_save_post_process which writes only
/// the fields it receives.
@inline(__always)
private func withOptionalCString<R>(_ s: String?,
                                     _ body: (UnsafePointer<CChar>?) -> R) -> R {
    guard let s, !s.isEmpty else { return body(nil) }
    return s.withCString { body($0) }
}

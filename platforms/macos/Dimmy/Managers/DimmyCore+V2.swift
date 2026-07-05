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

    /// Rebuild a meeting's `transcripts.txt` from the per-band audio
    /// (`audio_mic.*` / `audio_system.*`, or `audio.*` as mic fallback)
    /// in the live-worker format `[<ms> ms] [band] text`, interleaved by
    /// time. The Rust side writes `transcripts.txt` itself and returns
    /// the merged text. Blocking — call from a background thread.
    ///
    /// Return codes (negative = failure):
    ///   - .success(text) — merged transcript (empty if nothing produced)
    ///   - .failure(.invalidArgs)      — null pointer / bad UTF-8 (-1)
    ///   - .failure(.openFailed)       — no decodable band audio (-2)
    ///   - .failure(.silentInput)      — every band produced empty text (-5)
    ///   - .failure(.cloudUnsupported) — cloud STT config incomplete (-6)
    ///   - .failure(.backendFailed)    — write failed / backend error (other)
    func meetingRetranscribe(dir: String) -> Result<String, FileTranscribeError> {
        guard isInitialized else { return .failure(.notInitialized) }
        let bufLen: Int32 = 1 << 22  // 4 MB transcript buffer (long meetings)
        let buffer = UnsafeMutablePointer<CChar>.allocate(capacity: Int(bufLen))
        defer { buffer.deallocate() }
        buffer[0] = 0
        let rc = dir.withCString { p in dimmy_meeting_retranscribe(p, buffer, bufLen) }
        if rc < 0 {
            switch rc {
            case -1: return .failure(.invalidArgs)
            case -2: return .failure(.openFailed)
            case -5: return .failure(.silentInput)
            case -6: return .failure(.cloudUnsupported)
            default: return .failure(.backendFailed)
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
    /// nil if no meeting is active. Blocking for the worker's final
    /// chunk drain — typically ~1 s, but SECONDS-to-tens-of-seconds on
    /// long meetings with slow STT. Call from a background thread only.
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

    /// True while the active meeting is paused (worker is skipping WAV
    /// writes + STT chunks). False also when no meeting is active.
    var meetingIsPaused: Bool {
        dimmy_meeting_is_paused() == 1
    }

    /// Pause the in-flight meeting. Returns true iff the state actually
    /// flipped (was running, now paused). False = already paused or no
    /// meeting active. The Rust worker keeps cpal callbacks running; only
    /// WAV writes + STT chunks are skipped, and on resume the cursors
    /// advance past the gap so it doesn't land in audio.wav or the
    /// transcript timeline. A `[paused N ms]` marker is appended to
    /// transcripts.txt at the seam.
    @discardableResult
    func meetingPause() -> Bool {
        guard isInitialized else { return false }
        return dimmy_meeting_pause() == 1
    }

    /// Resume a paused meeting. Returns true iff the state actually
    /// flipped (was paused, now running).
    @discardableResult
    func meetingResume() -> Bool {
        guard isInitialized else { return false }
        return dimmy_meeting_resume() == 1
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

    // MARK: - Model catalog

    /// The embedded model catalog JSON (single source of truth for cloud
    /// models per provider). No `isInitialized` guard — the catalog is a
    /// compile-time constant in the core, available before `dimmy_init`.
    /// Sizes the buffer to the exact length the core reports so a growing
    /// catalog is never truncated.
    func modelCatalogJSON() -> String? {
        let needed = dimmy_model_catalog_json(nil, 0)
        guard needed > 0 else { return nil }
        let cap = Int(needed) + 1
        let buffer = UnsafeMutablePointer<CChar>.allocate(capacity: cap)
        defer { buffer.deallocate() }
        buffer[0] = 0
        let written = dimmy_model_catalog_json(buffer, Int32(cap))
        guard written > 0 else { return nil }
        return String(cString: buffer)
    }

    // MARK: - Recording consent

    /// Recording-consent notice text from the shared core. `kind` is "modal"
    /// (recorder confirmation) or "announcement" (TTS + chat message for the
    /// participants). `lang` is a BCP-47-ish tag; unsupported tags fall back to
    /// English in the core. No `isInitialized` guard for "modal" (pure), but
    /// "announcement" reads the live STT/LLM mode so call after init.
    func consentText(kind: String, lang: String) -> String? {
        let cap = 4096
        let buffer = UnsafeMutablePointer<CChar>.allocate(capacity: cap)
        defer { buffer.deallocate() }
        buffer[0] = 0
        let written = kind.withCString { k in
            lang.withCString { l in
                dimmy_consent_text(k, l, buffer, Int32(cap))
            }
        }
        guard written > 0 else { return nil }
        return String(cString: buffer)
    }

    /// Append a consent event ("confirmed" / "declined" / "announced" /
    /// "chat_copied") to the local audit log. Best-effort.
    func consentLogEvent(kind: String, lang: String) {
        _ = kind.withCString { k in
            lang.withCString { l in
                dimmy_consent_log_event(k, l)
            }
        }
    }

    /// Telemetry: a meeting was started/stopped from a shortcut or menu.
    /// `source` ∈ {"hotkey","menu","jumplist"} (categorical, no content).
    func trackMeetingAction(source: String) {
        source.withCString { s in
            dimmy_track_meeting_action(s)
        }
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
            case -4: return .failure(.localModelMissing)
            case -5: return .failure(.modelNotSupported)
            case -6: return .failure(.unauthorized)
            case -7: return .failure(.rateLimited)
            case -8: return .failure(.networkError)
            case -10: return .failure(.truncated)
            case -11: return .failure(.refused)
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
        case localModelMissing  // llm_mode=local but the picked Gemma .gguf isn't on disk
        case modelNotSupported  // -5: 404 from the recap endpoint (wrong model id for vendor)
        case unauthorized       // -6: 401/403 — bad / missing recap key
        case rateLimited        // -7: 429 — too many requests
        case networkError       // -8: socket / DNS / TLS error
        case truncated          // -10: output hit the token limit even after the 4x retry
        case refused            // -11: model declined on safety grounds (stop_reason=refusal)
        case unknown(Int)

        var description: String {
            switch self {
            case .notInitialized: return "Core not initialized"
            case .emptyPrompt: return "Empty prompt"
            case .invalidArgs: return "Invalid arguments"
            case .notConfigured: return "LLM API URL or key is not configured"
            case .httpError: return "LLM HTTP / parse error — see dimmy.log"
            case .localModelMissing: return "Local LLM model not downloaded — open Settings → LLM and download a Gemma variant"
            case .modelNotSupported: return "Recap model not supported by the recap endpoint — pick a different model in Settings → Recap"
            case .unauthorized: return "Recap API key is missing or unauthorized — open Settings → Recap"
            case .rateLimited: return "Recap rate limited (429) — try again in a minute"
            case .networkError: return "Network error reaching the recap endpoint — check connection"
            case .truncated: return "The recap was cut off at the model token limit even after a retry. Pick a larger-context model."
            case .refused: return "The model declined to process this content. Reword or pick a different model."
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

    // MARK: - Notion integration
    //
    // Mirrors the Win side. Token stored in the AES-256 keystore via
    // dimmy_notion_set_token; token presence + target round-trip via
    // get_config_json so the Settings UI can render the connected /
    // disconnected state without re-pinging Notion.

    /// Save the user's Notion integration token. Empty string clears.
    /// Returns true on success.
    @discardableResult
    func notionSetToken(_ token: String) -> Bool {
        guard isInitialized else { return false }
        return token.withCString { ptr in
            dimmy_notion_set_token(ptr) == 0
        }
    }

    /// True if a Notion token is stored (regardless of validity).
    var notionHasToken: Bool {
        guard isInitialized else { return false }
        return dimmy_notion_has_token() == 1
    }

    /// Ping /v1/users/me with the stored token. Returns the JSON
    /// envelope `{"ok":true|false, "bot_name":"...", "workspace_name":"...", "error":"..."}`
    /// raw — the caller decodes.
    func notionTestConnection() -> String? {
        guard isInitialized else { return nil }
        var buffer = [CChar](repeating: 0, count: 16384)
        let len = buffer.withUnsafeMutableBufferPointer { ptr -> Int32 in
            dimmy_notion_test_connection(ptr.baseAddress!, Int32(ptr.count))
        }
        guard len > 0 else { return nil }
        return String(cString: buffer)
    }

    /// Search Notion for accessible pages + databases. Returns the raw
    /// JSON array (or `{"error":"..."}` on failure); caller decodes.
    func notionSearch(_ query: String) -> String? {
        guard isInitialized else { return nil }
        return query.withCString { qptr -> String? in
            var buffer = [CChar](repeating: 0, count: 32768)
            let len = buffer.withUnsafeMutableBufferPointer { ptr -> Int32 in
                dimmy_notion_search(qptr, ptr.baseAddress!, Int32(ptr.count))
            }
            guard len > 0 else { return nil }
            return String(cString: buffer)
        }
    }

    /// Send the meeting's recap.md to the configured Notion target.
    /// Returns the raw JSON envelope; caller decodes ok/page_url/error.
    func notionSendRecap(meetingDir: String) -> String? {
        guard isInitialized else { return nil }
        return meetingDir.withCString { dptr -> String? in
            var buffer = [CChar](repeating: 0, count: 16384)
            let len = buffer.withUnsafeMutableBufferPointer { ptr -> Int32 in
                dimmy_notion_send_recap(dptr, ptr.baseAddress!, Int32(ptr.count))
            }
            guard len > 0 else { return nil }
            return String(cString: buffer)
        }
    }

    // ── User dictionary (custom vocabulary biasing) ──────────────────

    /// Result of `userDictAdd` — mirrors the FFI rc table so callers can
    /// branch on "added vs already-present vs error" without remembering
    /// magic numbers. Win's DictionaryService.Add returns the raw int
    /// 0/1/-1; we lift that into an enum here so SwiftUI binding paths
    /// stay strongly typed.
    enum UserDictAddResult {
        case added
        case alreadyPresent
        case error
    }

    /// Append `word` to the user dictionary. The Rust side persists to
    /// config.json + case-insensitive dedupes. Empty / whitespace-only
    /// strings are rejected (returns .error) without touching state —
    /// callers should still trim defensively. Safe to call from any
    /// thread.
    @discardableResult
    func userDictAdd(_ word: String) -> UserDictAddResult {
        guard isInitialized else { return .error }
        let trimmed = word.trimmingCharacters(in: .whitespacesAndNewlines)
        if trimmed.isEmpty { return .error }
        let rc = trimmed.withCString { ptr in dimmy_user_dict_add(ptr) }
        switch rc {
        case 0: return .added
        case 1: return .alreadyPresent
        default: return .error
        }
    }

    /// Remove all entries matching `word` (case-insensitive). Returns
    /// the count of entries dropped. 0 means nothing matched; negative
    /// means an FFI / persistence error.
    @discardableResult
    func userDictRemove(_ word: String) -> Int {
        guard isInitialized else { return -1 }
        let trimmed = word.trimmingCharacters(in: .whitespacesAndNewlines)
        if trimmed.isEmpty { return -1 }
        let rc = trimmed.withCString { ptr in dimmy_user_dict_remove(ptr) }
        return Int(rc)
    }

    /// Read the current dictionary as `[String]`. Returns empty array
    /// on FFI failure (the dictionary just isn't populated yet — callers
    /// don't need to differentiate "error" from "empty").
    func userDictList() -> [String] {
        guard isInitialized else { return [] }
        var buffer = [CChar](repeating: 0, count: 16384)
        let len = buffer.withUnsafeMutableBufferPointer { ptr -> Int32 in
            dimmy_user_dict_list_json(ptr.baseAddress!, Int32(ptr.count))
        }
        guard len > 0 else { return [] }
        let json = String(cString: buffer)
        guard let data = json.data(using: .utf8),
              let arr = try? JSONSerialization.jsonObject(with: data) as? [String] else {
            return []
        }
        return arr
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

// MARK: - DimmyCore — multi-format audio decode + peaks

extension DimmyCore {
    /// Compute waveform peaks for any decodable audio file (WAV/Ogg/etc.
    /// via Symphonia in the Rust core). Returns nil on FFI failure; the
    /// caller falls back to "no waveform" (drawing an empty band).
    /// Used by `WavPeaks.readPeaks` when the path is .ogg — WAV stays on
    /// the in-Swift parser which is ~5× faster for the common case.
    func computeAudioPeaks(at path: String, bucketCount: Int) -> [Float]? {
        guard isInitialized, bucketCount > 0 else { return nil }
        // 64 KB JSON buffer — enough for 4096 buckets × ~13 bytes/peak +
        // metadata. The FFI returns -3 if not enough; we don't retry,
        // we just degrade to "no peaks" (same as a decode failure).
        let bufLen: Int32 = 64 * 1024
        let buffer = UnsafeMutablePointer<CChar>.allocate(capacity: Int(bufLen))
        defer { buffer.deallocate() }
        buffer[0] = 0
        let rc = path.withCString { p in
            dimmy_compute_audio_peaks(p, Int32(bucketCount), buffer, bufLen)
        }
        guard rc > 0 else { return nil }
        let json = String(cString: buffer)
        guard let data = json.data(using: .utf8),
              let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let raw = obj["peaks"] as? [Any]
        else { return nil }
        var out = [Float](repeating: 0, count: raw.count)
        for (i, v) in raw.enumerated() {
            if let d = v as? Double { out[i] = Float(d) }
            else if let n = v as? NSNumber { out[i] = n.floatValue }
        }
        return out
    }

    /// Decode an audio file (any format the Rust loader supports) to a
    /// mono int16 WAV on disk at the source's native sample rate. Used
    /// by AudioPlaybackBar so AVAudioPlayer — which doesn't natively
    /// decode Ogg/Vorbis on macOS — can still play `audio.ogg` meetings
    /// via a one-shot tmp WAV. Returns true on success; the caller is
    /// expected to delete `dst` when the player is torn down.
    @discardableResult
    func decodeAudioToWav(source: String, destination: String) -> Bool {
        guard isInitialized else { return false }
        let rc = source.withCString { s in
            destination.withCString { d in
                dimmy_decode_audio_to_wav(s, d)
            }
        }
        return rc > 0
    }
}

//
//  DimmyFFI.h
//  Dimmy
//
//  C bridging header for the Rust FFI layer (ffi.rs).
//  All functions are defined in libdimmy_lib.a.
//

#ifndef DimmyFFI_h
#define DimmyFFI_h

#include <stdint.h>

// ── Lifecycle ───────────────────────────────────────────────────────

/// Initialize the Dimmy core. Must be called once before any other function.
/// Returns 0 on success, -1 on error.
int32_t dimmy_init(void);

/// Shut down: stop audio, save config and clean up.
void dimmy_shutdown(void);

/// Register event callback. The native UI provides a function pointer that
/// receives JSON strings for events (recording_progress, chunk_status, etc.).
void dimmy_set_event_callback(void (*_Nonnull cb)(const char * _Nonnull));

// ── Recording ───────────────────────────────────────────────────────

/// Start recording. Returns 0=OK, -1=no API key, -2=already recording.
int32_t dimmy_start_recording(void);

/// Stop recording and get transcript. Returns transcript length, or negative on error.
/// Transcript is written to out_buf (null-terminated).
int32_t dimmy_stop_recording(char * _Nonnull out_buf, int32_t buf_len);

/// Cancel recording without transcribing.
void dimmy_cancel_recording(void);

// ── Config ──────────────────────────────────────────────────────────

/// Get full config as JSON string. Returns length written, or -1 on error.
int32_t dimmy_get_config_json(char * _Nonnull out_buf, int32_t buf_len);

/// Set config from JSON string. Returns 0=OK, -1=error.
int32_t dimmy_set_config_json(const char * _Nonnull json_ptr);

// ── Audio ───────────────────────────────────────────────────────────

/// Get current microphone amplitude (0.0 - 1.0).
float dimmy_get_amplitude(void);

/// Get device list as JSON array. Returns length written, or -1 on error.
int32_t dimmy_list_devices_json(char * _Nonnull out_buf, int32_t buf_len);

/// Check audio device health. Returns JSON with diagnostic info.
int32_t dimmy_check_audio_health(char * _Nonnull out_buf, int32_t buf_len);

// ── LLM ─────────────────────────────────────────────────────────────

/// Cycle LLM style. direction: +1 = next, -1 = previous.
void dimmy_cycle_llm_style(int32_t direction);

/// Cycle LLM tone. direction: +1 = next, -1 = previous.
void dimmy_cycle_llm_tone(int32_t direction);

/// Process text through LLM enhancement. Returns length written to buffer,
/// or -1 on error, 0 if LLM disabled/style=Off.
int32_t dimmy_process_with_llm(const char * _Nonnull text_ptr,
                               char * _Nonnull out_buf,
                               int32_t buf_len);

// ── Stats ───────────────────────────────────────────────────────────

/// Update cumulative stats. Returns 0=OK, -1=invalid input.
int32_t dimmy_update_stats(int32_t words, double speaking_secs);

// ── Utility ─────────────────────────────────────────────────────────

/// Check if an API key is configured. Returns 1=yes, 0=no.
int32_t dimmy_has_api_key(void);

/// Check if recording is active. Returns 1=yes, 0=no.
int32_t dimmy_is_recording(void);

// ── Local STT ──────────────────────────────────────────────────────

/// Get JSON array of available local models with download status.
/// Returns bytes written, or -1 on error.
int32_t dimmy_list_local_models(char * _Nonnull out_buf, int32_t buf_len);

/// Download a model file. BLOCKING — call from a background thread.
/// Emits "model_download_progress" events via the event callback.
/// Returns 0 on success, -1 on error.
int32_t dimmy_download_model(const char * _Nonnull filename);

/// Check if a specific model file exists locally.
/// Returns 1=yes, 0=no.
int32_t dimmy_model_exists(const char * _Nonnull filename);

// ── Transcription History ──────────────────────────────────────────

/// Save a transcript to history. Returns transcript ID, or -1 on error.
int32_t dimmy_history_save(const char * _Nonnull text,
                           const char * _Nullable language,
                           double duration);

/// Get recent transcripts as JSON array. Returns bytes written, or -1 on error.
int32_t dimmy_history_recent(int32_t limit,
                             char * _Nonnull out_buf,
                             int32_t buf_len);

/// Search transcripts via full-text search. Returns bytes written, or -1 on error.
int32_t dimmy_history_search(const char * _Nonnull query,
                             int32_t limit,
                             char * _Nonnull out_buf,
                             int32_t buf_len);

/// Delete a transcript by ID. Returns 0=OK, -1=error.
int32_t dimmy_history_delete(int32_t id);

/// Get history stats as JSON. Returns bytes written, or -1 on error.
int32_t dimmy_history_stats(char * _Nonnull out_buf, int32_t buf_len);

#endif /* DimmyFFI_h */

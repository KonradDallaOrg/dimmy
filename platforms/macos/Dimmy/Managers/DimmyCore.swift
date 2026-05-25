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

    /// True once `initialize()` has returned success. FFI calls panic
    /// in Rust if invoked before `dimmy_init()`; callers gate on this
    /// flag when the core may not yet be ready (e.g. permissions still
    /// pending so `initializeCoreAsync` hasn't fired).
    private(set) var isInitialized: Bool = false

    // MARK: - ONNX Runtime dylib path

    /// Resolve and export ORT_DYLIB_PATH BEFORE the first FFI call. The
    /// `ort` crate is built with the `load-dynamic` feature, so the
    /// Parakeet path opens libonnxruntime.dylib via this env var on
    /// first use. Lookup chain (first hit wins):
    ///   1. caller-provided env var (CI / dev)
    ///   2. Frameworks folder of the running .app (release / `Cmd+R`)
    ///   3. repo dev fallback core/target/onnxruntime-osx-arm64-<ver>/lib/
    /// If none resolve we leave the var unset; ort will surface a clear
    /// "library not loaded" only when Parakeet is actually invoked,
    /// keeping non-Parakeet flows unaffected.
    private static func configureOrtDylibPath() {
        if let existing = ProcessInfo.processInfo.environment["ORT_DYLIB_PATH"],
           !existing.isEmpty {
            return
        }
        let fm = FileManager.default
        let candidates: [String] = [
            Bundle.main.privateFrameworksPath.map { "\($0)/libonnxruntime.dylib" },
            Bundle.main.bundlePath.appending("/Contents/Frameworks/libonnxruntime.dylib"),
            devFallbackOrtPath(),
        ].compactMap { $0 }
        for path in candidates where fm.fileExists(atPath: path) {
            setenv("ORT_DYLIB_PATH", path, 1)
            print("[DimmyCore] ORT_DYLIB_PATH=\(path)")
            return
        }
        print("[DimmyCore] ORT_DYLIB_PATH unresolved (Parakeet path will fail until set)")
    }

    private static func devFallbackOrtPath() -> String? {
        // Walk up from the app bundle to the repo root and pick the
        // pinned arm64 dylib that scripts/download-onnxruntime.sh drops
        // under core/target/. Only used during `swift build` / Xcode
        // run-from-source; release .app installs hit the Frameworks
        // path above first.
        let bundlePath = Bundle.main.bundlePath
        let parts = bundlePath.split(separator: "/").map(String.init)
        guard parts.count >= 4 else { return nil }
        // Trim platforms/macos/build/.../Dimmy.app — repo root is 5+ levels up.
        // Conservative: try a few ancestor depths and pick the first hit.
        var candidate = (bundlePath as NSString).deletingLastPathComponent
        for _ in 0..<8 {
            let probe = "\(candidate)/core/target/onnxruntime-osx-arm64-1.22.0/lib/libonnxruntime.dylib"
            if FileManager.default.fileExists(atPath: probe) {
                return probe
            }
            candidate = (candidate as NSString).deletingLastPathComponent
            if candidate == "/" || candidate.isEmpty { break }
        }
        return nil
    }

    // MARK: - Lifecycle

    /// Initialize the Rust core. Call once at app launch.
    /// Returns true on success.
    @discardableResult
    func initialize() -> Bool {
        Self.configureOrtDylibPath()
        let result = dimmy_init()
        if result == 0 {
            isInitialized = true
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
        // Hard guard: dimmy_set_config_json calls into state() which
        // panics with "dimmy_init() must be called before any other
        // function" when the Rust core hasn't finished initializing.
        // Without this guard, any UI interaction that happens during
        // the init window (~50 ms on this dev box, longer with cold
        // CoreML compile) hits the panic and SIGABRTs the whole app.
        // Hit live by scrolling the pill style dot in the first
        // moment after launch — backtrace landed unwrap_failed →
        // dimmy_set_config_json → DimmyCore.setConfig → cycleStyle.
        guard isInitialized else {
            print("[DimmyCore] setConfig dropped — core not yet initialized")
            return false
        }
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

    /// Peak amplitude of the secondary (system / loopback) audio buffer.
    /// 0.0 when no Mix-mode meeting is in flight. The Mac meeting window
    /// pairs this with `getAmplitude()` to draw a dual-band waveform.
    func getLoopbackAmplitude() -> Float {
        dimmy_get_loopback_amplitude()
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

    // MARK: - Telemetry

    /// Whether PostHog analytics emission is currently enabled.
    var telemetryEnabled: Bool {
        get { dimmy_telemetry_is_enabled() == 1 }
        set { _ = dimmy_telemetry_set_enabled(newValue ? 1 : 0) }
    }

    /// Whether Sentry crash reporting is currently enabled.
    var crashReportingEnabled: Bool {
        get { dimmy_telemetry_is_crash_enabled() == 1 }
        set { _ = dimmy_telemetry_set_crash_enabled(newValue ? 1 : 0) }
    }

    /// Anonymous distinct_id (UUID) reported to PostHog. Empty if telemetry off.
    var telemetryAnonymousId: String {
        let bufLen = Self.bufferSize
        let buffer = UnsafeMutablePointer<CChar>.allocate(capacity: Int(bufLen))
        defer { buffer.deallocate() }
        buffer[0] = 0
        let written = dimmy_telemetry_anonymous_id(buffer, bufLen)
        guard written > 0 else { return "" }
        return String(cString: buffer)
    }

    /// Wipe + regenerate the anonymous_id (the "Reset analytics ID" button).
    func resetTelemetryAnonymousId() {
        _ = dimmy_telemetry_reset_anonymous_id()
    }

    /// Full telemetry status as a parsed JSON dict.
    func telemetryStatus() -> [String: Any]? {
        let bufLen = Self.bufferSize
        let buffer = UnsafeMutablePointer<CChar>.allocate(capacity: Int(bufLen))
        defer { buffer.deallocate() }
        buffer[0] = 0
        let written = dimmy_telemetry_status(buffer, bufLen)
        guard written > 0 else { return nil }
        let jsonStr = String(cString: buffer)
        guard let data = jsonStr.data(using: .utf8),
              let dict = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        else { return nil }
        return dict
    }

    /// Send a user feedback message through the Rust telemetry pipeline.
    /// Best-effort: returns true if the FFI accepted the call (queued for
    /// send), false on a precondition error. Empty messages are no-ops.
    @discardableResult
    func captureFeedback(kind: String, message: String, email: String?) -> Bool {
        guard !message.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            return true  // matches Rust: empty feedback = silent no-op
        }
        let result = kind.withCString { kindPtr in
            message.withCString { msgPtr in
                if let email, !email.isEmpty {
                    return email.withCString { emailPtr in
                        dimmy_telemetry_capture_feedback(kindPtr, msgPtr, emailPtr)
                    }
                }
                return dimmy_telemetry_capture_feedback(kindPtr, msgPtr, nil)
            }
        }
        return result == 0
    }

    // MARK: - Autostart

    /// Whether Dimmy is registered to launch at login (auto-launch).
    var autostartEnabled: Bool {
        get { dimmy_autostart_is_enabled() == 1 }
        set { _ = dimmy_autostart_set_enabled(newValue ? 1 : 0) }
    }

    // MARK: - Diagnostics (audio probe lives at line 144 above)

    /// Build version reported by the Rust core (CARGO_PKG_VERSION).
    var coreVersion: String {
        let bufLen: Int32 = 64
        let buffer = UnsafeMutablePointer<CChar>.allocate(capacity: Int(bufLen))
        defer { buffer.deallocate() }
        buffer[0] = 0
        let written = dimmy_get_version(buffer, bufLen)
        guard written > 0 else { return "" }
        return String(cString: buffer)
    }

    /// Build flavor — "" (prod) or "staging". Drives the staging banner +
    /// title suffix in the SwiftUI views so a side-by-side tester can't
    /// confuse one flavor for the other. Mirrors Win BuildInfo.Flavor.
    var buildFlavor: String {
        let bufLen: Int32 = 64
        let buffer = UnsafeMutablePointer<CChar>.allocate(capacity: Int(bufLen))
        defer { buffer.deallocate() }
        buffer[0] = 0
        let written = dimmy_build_flavor(buffer, bufLen)
        guard written > 0 else { return "" }
        return String(cString: buffer)
    }

    /// True when this binary was built with `DIMMY_BUILD_FLAVOR=staging`.
    var isStagingBuild: Bool { buildFlavor == "staging" }

    /// Folder under Application Support that holds config.json,
    /// history.db, license.json, meetings/, dimmy.log. Reads the Rust
    /// core's compile-time `config_dir_name()` (DIMMY_CONFIG_NAMESPACE,
    /// default "dimmy") via FFI — NEVER derive it from the build flavor.
    ///
    /// Flavor and config-dir are decoupled (2026-05-16): a flavor=staging
    /// build that ships under the prod packId keeps the prod `dimmy` dir,
    /// while only the side-by-side tester build sets `dimmy-staging`.
    /// Deriving from `isStagingBuild` made the app read/write under
    /// `dimmy-staging` while the core wrote under `dimmy` — meetings
    /// "vanished" and the bundled MCP looked in the wrong dir. Always use
    /// this helper from Swift, never `appendingPathComponent("dimmy")`.
    var configDirName: String {
        let bufLen: Int32 = 128
        let buffer = UnsafeMutablePointer<CChar>.allocate(capacity: Int(bufLen))
        defer { buffer.deallocate() }
        buffer[0] = 0
        let written = dimmy_config_dir_name(buffer, bufLen)
        guard written > 0 else { return "dimmy" }
        let name = String(cString: buffer)
        return name.isEmpty ? "dimmy" : name
    }

    /// Full URL of the config directory under Application Support.
    /// Convenience wrapper; nil only on bizarre systems where the
    /// Application Support directory itself cannot be located.
    var configDirURL: URL? {
        FileManager.default.urls(
            for: .applicationSupportDirectory,
            in: .userDomainMask
        ).first?.appendingPathComponent(configDirName, isDirectory: true)
    }

    // ── Claude Code subscription login ───────────────────────────
    //
    // Use the user's Anthropic Pro/Team/Max plan via the local
    // `claude` CLI instead of API-key credit. See
    // core/src/claude_code.rs for the design.

    enum ClaudeCodeStatus: Int32 {
        case ready = 0
        case notLoggedIn = 1
        case notInstalled = 2
    }

    /// Probe local Claude Code state. Cheap — no subprocess.
    var claudeCodeStatus: ClaudeCodeStatus {
        let raw = dimmy_claude_code_status()
        return ClaudeCodeStatus(rawValue: raw) ?? .notInstalled
    }

    /// Resolved path of the `claude` binary, or nil if not installed.
    var claudeCodeBinaryPath: String? {
        let bufLen: Int32 = 4096
        let buffer = UnsafeMutablePointer<CChar>.allocate(capacity: Int(bufLen))
        defer { buffer.deallocate() }
        buffer[0] = 0
        let written = dimmy_claude_code_binary_path(buffer, bufLen)
        guard written > 0 else { return nil }
        return String(cString: buffer)
    }

    /// Spawn `claude /login` in a new Terminal window (via
    /// osascript). Returns true on success. Caller polls
    /// `claudeCodeStatus` to detect login completion.
    @discardableResult
    func spawnClaudeCodeLogin() -> Bool {
        return dimmy_claude_code_spawn_login() == 0
    }

    /// Outcome of the Test Connection ping. Mirrors the Win
    /// `ClaudeCodePingResult` enum and the Rust FFI return-code table.
    enum ClaudeCodePingResult {
        case ok(elapsedMs: Int32)
        case notInstalled
        case notLoggedIn
        case spawnFailed
        case timeout
        case nonZeroExit
        case invalidUtf8
        case unknownError
    }

    /// Run a content-free "ping" prompt through the local CLI.
    /// BLOCKING — call from a background queue.
    func pingClaudeCode() -> ClaudeCodePingResult {
        let rc = dimmy_claude_code_ping()
        if rc > 0 { return .ok(elapsedMs: rc) }
        switch rc {
        case -1: return .notInstalled
        case -2: return .notLoggedIn
        case -3: return .spawnFailed
        case -4: return .timeout
        case -5: return .nonZeroExit
        case -6: return .invalidUtf8
        default: return .unknownError
        }
    }

    // ── Setup wizard support (Node.js detection + cache recheck) ──

    /// Node.js detection snapshot for the setup wizard's Step 1.
    /// Mirror of the Rust JSON envelope.
    struct NodeStatus {
        let found: Bool
        let path: String?
        let version: String?
        let major: Int?
        let meetsMinimum: Bool

        static let missing = NodeStatus(
            found: false, path: nil, version: nil,
            major: nil, meetsMinimum: false)
    }

    /// Probe for a local Node.js install. Returns a structured snapshot
    /// the setup wizard binds to. `meetsMinimum` is true iff
    /// `major >= 18` (claude-code's minimum runtime).
    func nodeStatus() -> NodeStatus {
        let bufLen: Int32 = 2048
        let buffer = UnsafeMutablePointer<CChar>.allocate(capacity: Int(bufLen))
        defer { buffer.deallocate() }
        buffer[0] = 0
        let written = dimmy_claude_code_node_status(buffer, bufLen)
        guard written > 0,
              let data = String(cString: buffer).data(using: .utf8),
              let raw = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            return .missing
        }
        let found = raw["found"] as? Bool ?? false
        if !found { return .missing }
        return NodeStatus(
            found: true,
            path: raw["path"] as? String,
            version: raw["version"] as? String,
            major: raw["major"] as? Int,
            meetsMinimum: raw["meets_minimum"] as? Bool ?? false)
    }

    /// Invalidate cached binary lookups (claude + node) and return
    /// the fresh Claude Code status. Used by the wizard's "I installed
    /// it, recheck" buttons — without this, Dimmy keeps stale "not
    /// found" answers until restart.
    @discardableResult
    func recheckClaudeCode() -> ClaudeCodeStatus {
        let raw = dimmy_claude_code_recheck()
        return ClaudeCodeStatus(rawValue: raw) ?? .notInstalled
    }

    // MARK: - Claude Desktop MCP bridge

    /// One-shot snapshot of the MCP bridge state. Mirror of the Rust
    /// `dimmy_claude_desktop_status` JSON envelope. Every field is
    /// optional — "present" means "fact known", "absent" means the
    /// host should render a neutral / probing state. `configPatched`
    /// is a legacy alias of `extensionInstalled` kept for code paths
    /// that haven't migrated yet; new code reads extensionInstalled.
    struct ClaudeDesktopStatus {
        let installed: Bool
        let installPath: String?
        let extensionPath: String?
        let extensionInstalled: Bool
        let extensionEnabled: Bool
        let extensionVersion: String?
        let entryCommand: String?
        let heartbeatAgeSecs: Int64?
        let lastCallTool: String?
        let lastCallAgoSecs: Int64?
        let lastCallOk: Bool?
        let lastCallElapsedMs: Int64?

        var configPatched: Bool { extensionInstalled }

        static let empty = ClaudeDesktopStatus(
            installed: false, installPath: nil, extensionPath: nil,
            extensionInstalled: false, extensionEnabled: false,
            extensionVersion: nil, entryCommand: nil,
            heartbeatAgeSecs: nil, lastCallTool: nil,
            lastCallAgoSecs: nil, lastCallOk: nil, lastCallElapsedMs: nil)
    }

    func claudeDesktopStatus() -> ClaudeDesktopStatus {
        let bufSize = 2048
        var buf = [CChar](repeating: 0, count: bufSize)
        let n = dimmy_claude_desktop_status(&buf, Int32(bufSize))
        guard n > 0 else { return .empty }
        let json = String(cString: buf)
        guard let data = json.data(using: .utf8),
              let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        else { return .empty }

        let installed = obj["installed"] as? Bool ?? false
        let installPath = obj["install_path"] as? String
        let extPath = obj["extension_path"] as? String
        let extInstalled = obj["extension_installed"] as? Bool ?? false
        let extEnabled = obj["extension_enabled"] as? Bool ?? false
        let extVersion = obj["extension_version"] as? String
        let entryCmd = obj["entry_command"] as? String
        let hbAge = (obj["heartbeat_age_secs"] as? NSNumber)?.int64Value

        var lcTool: String? = nil
        var lcAgo: Int64? = nil
        var lcOk: Bool? = nil
        var lcMs: Int64? = nil
        if let lc = obj["last_call"] as? [String: Any] {
            lcTool = lc["tool"] as? String
            lcAgo = (lc["ago_secs"] as? NSNumber)?.int64Value
            lcOk = lc["ok"] as? Bool
            lcMs = (lc["elapsed_ms"] as? NSNumber)?.int64Value
        }

        return ClaudeDesktopStatus(
            installed: installed, installPath: installPath,
            extensionPath: extPath, extensionInstalled: extInstalled,
            extensionEnabled: extEnabled, extensionVersion: extVersion,
            entryCommand: entryCmd, heartbeatAgeSecs: hbAge,
            lastCallTool: lcTool, lastCallAgoSecs: lcAgo,
            lastCallOk: lcOk, lastCallElapsedMs: lcMs)
    }

    /// Install Dimmy as a Claude Desktop extension. Copies the
    /// dimmy-mcp binary into the user's Claude Extensions dir, drops
    /// a manifest.json + icon.png next to it, flips the per-extension
    /// enable flag. Idempotent. Returns true on success.
    func installClaudeDesktopExtension(binaryPath: String, version: String) -> Bool {
        return binaryPath.withCString { binPtr in
            version.withCString { verPtr in
                dimmy_claude_desktop_install(binPtr, verPtr) == 0
            }
        }
    }

    /// Remove the Dimmy Claude Desktop extension dir + its settings
    /// file. Returns true iff something was actually removed.
    func uninstallClaudeDesktopExtension() -> Bool {
        return dimmy_claude_desktop_uninstall() == 1
    }

    /// Emit a typed telemetry event. `props` is a Swift dict that
    /// will be JSON-encoded and passed to the Rust dispatcher.
    /// Non-categorical or unknown event names silently drop (-2).
    func trackEvent(_ name: String, _ props: [String: Any] = [:]) {
        let propsJson: String
        if props.isEmpty {
            propsJson = ""
        } else if let data = try? JSONSerialization.data(withJSONObject: props),
                  let json = String(data: data, encoding: .utf8) {
            propsJson = json
        } else {
            return
        }
        name.withCString { namePtr in
            if propsJson.isEmpty {
                _ = dimmy_telemetry_track_typed(namePtr, nil)
            } else {
                propsJson.withCString { propsPtr in
                    _ = dimmy_telemetry_track_typed(namePtr, propsPtr)
                }
            }
        }
    }

    /// GPU known-bad status as parsed JSON. `enabled` indicates whether
    /// the marker is currently set (forcing CPU fallback).
    func gpuStatus() -> [String: Any]? {
        let bufLen: Int32 = 4096
        let buffer = UnsafeMutablePointer<CChar>.allocate(capacity: Int(bufLen))
        defer { buffer.deallocate() }
        buffer[0] = 0
        let written = dimmy_gpu_get_status(buffer, bufLen)
        guard written > 0 else { return nil }
        let jsonStr = String(cString: buffer)
        guard let data = jsonStr.data(using: .utf8),
              let dict = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        else { return nil }
        return dict
    }

    /// Clear the GPU known-bad marker so Metal is re-probed next launch.
    func gpuClearKnownBad() {
        _ = dimmy_gpu_clear_known_bad()
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

    // MARK: - Parakeet (alternative local STT backend)

    /// 1 = bundle complete on disk, 0 = missing or partial.
    func parakeetBundlePresent() -> Bool {
        dimmy_parakeet_bundle_present() == 1
    }

    /// Download the ~2.5 GB Parakeet bundle into the dimmy config dir.
    /// BLOCKING — call from a background thread. Progress arrives as
    /// "parakeet_bundle_download_progress" events on the global handler.
    @discardableResult
    func downloadParakeetBundle() -> Bool {
        let result = dimmy_parakeet_download_bundle()
        if result != 0 {
            print("[DimmyCore] ERROR: downloadParakeetBundle failed with code \(result)")
        }
        return result == 0
    }

    /// Pre-load the Parakeet sessions on a background thread so the
    /// user's first real recording doesn't pay the ~6 s cold path.
    /// No-op if the bundle isn't present yet — caller doesn't need
    /// to gate.
    @discardableResult
    func warmupParakeet() -> Bool {
        let result = dimmy_parakeet_warmup()
        if result != 0 {
            print("[DimmyCore] parakeet warmup skipped (rc=\(result))")
        }
        return result == 0
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
        // Re-read config so the cumulative word/speaking-time counters
        // shown in Home stay in sync after every successful transcription.
        // The Rust core writes the updated stats to config inside
        // `dimmy_stop_recording`; we just pull them back into AppState.
        if let cfg = DimmyCore.shared.getConfig() {
            appState.loadFromRustConfig(cfg)
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

    case "meeting_state":
        // Replaces the 0.5 s `meetingStatePollTimer` on PillWindowController.
        // Rust emits this exactly once per state transition (start /
        // pause / resume / stop), so we get instant updates with zero
        // idle CPU. See CLAUDE.md "No FFI-state polling rule".
        let active = (payload["active"] as? Bool) ?? false
        let paused = (payload["paused"] as? Bool) ?? false
        if appState.meetingActive != active { appState.meetingActive = active }
        if appState.meetingIsPaused != paused { appState.meetingIsPaused = paused }

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

    case "parakeet_bundle_download_progress":
        if let downloaded = payload["downloaded"] as? Int,
           let total = payload["total"] as? Int,
           total > 0 {
            appState.parakeetDownloadProgress = Double(downloaded) / Double(total)
        }

    case "file_transcribe_progress":
        // Emitted between chunks by dimmy_transcribe_file. Drives the
        // determinate progress bar on Settings → Home → Transcribe a file.
        let processed = (payload["processed_secs"] as? Double) ?? 0
        let total = (payload["total_secs"] as? Double) ?? 0
        let percent = (payload["percent"] as? Double) ?? 0
        let chunkIndex = (payload["chunk_index"] as? Int) ?? 0
        let chunkTotal = (payload["chunk_total"] as? Int) ?? 0
        appState.fileTranscribeProgress = FileTranscribeProgress(
            processedSecs: processed,
            totalSecs: total,
            percent: percent,
            chunkIndex: chunkIndex,
            chunkTotal: chunkTotal
        )

    case "stt_chunk":
        // Rust core emits this when chunk_streaming is on AND the
        // active local backend is Parakeet. Payload schema:
        //   { "delta": "<new text>", "cumulative": "<full text>",
        //     "is_final": <bool> }
        // CaptionWindowController watches the publishers below.
        let delta = (payload["delta"] as? String) ?? ""
        let cumulative = (payload["cumulative"] as? String) ?? ""
        let isFinal = (payload["is_final"] as? Bool) ?? false
        appState.liveCaptionDelta = delta
        appState.liveCaptionCumulative = cumulative
        appState.liveCaptionIsFinal = isFinal
        appState.liveCaptionTick &+= 1

    case "meeting_chunk":
        // Emitted by the Rust meeting worker every time it processes
        // a chunk (~15 s cadence). MeetingViewModel listens to the
        // published mirror so its live-transcript view updates as
        // chunks land — no transcripts.txt polling needed.
        // Payload:
        //   { "dir": "<abs path>", "speaker": "mic" | "system",
        //     "elapsed_ms": <u64>, "chunk_count": <u32>,
        //     "line": "[  12000 ms] [mic] hello\n" }
        let dir = (payload["dir"] as? String) ?? ""
        let speaker = (payload["speaker"] as? String) ?? ""
        let line = (payload["line"] as? String) ?? ""
        let chunkCount = (payload["chunk_count"] as? Int) ?? 0
        // Drop events from a stale recording — happens briefly when
        // the user starts a new meeting before the previous worker
        // has fully drained.
        if !appState.meetingActiveDir.isEmpty
            && appState.meetingActiveDir != dir {
            break
        }
        if appState.meetingActiveDir.isEmpty {
            appState.meetingActiveDir = dir
        }
        appState.meetingLiveTranscript.append(line)
        appState.meetingChunkCount = chunkCount
        appState.meetingLastChunkSpeaker = speaker
        appState.meetingChunkTick &+= 1

    case "call_detected":
        let app = payload["app"] as? String
        let since = (payload["since_seconds"] as? Int) ?? 0
        appState.onCallDetected(app: app, sinceSecs: since)

    case "call_ended":
        let app = payload["app"] as? String
        appState.onCallEnded(app: app)

    case "meeting.stop_suggested":
        let app = payload["app"] as? String
        let inactive = (payload["inactive_for_secs"] as? Int) ?? 0
        appState.onCallStopSuggested(app: app, inactiveSecs: inactive)

    default:
        print("[DimmyCore] unhandled event: \(event)")
    }
}

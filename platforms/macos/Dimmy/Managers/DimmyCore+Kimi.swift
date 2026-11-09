import Foundation

/// Kimi Code CLI (Kimi membership) FFI, sibling of `DimmyCore+GeminiCli.swift`.
///
/// Reuses `ClaudeCodeStatus` unchanged: the three states and their integer
/// codes are the same, pinned on the Rust side (`core/src/kimi_cli.rs`).
extension DimmyCore {

    /// Probe local Kimi CLI state. Cheap: no subprocess.
    var kimiStatus: ClaudeCodeStatus {
        ClaudeCodeStatus(rawValue: dimmy_kimi_status()) ?? .notInstalled
    }

    var kimiBinaryPath: String? {
        readKimiBuffer(4096) { dimmy_kimi_binary_path($0, $1) }
    }

    /// The vendor's one-line install command (`curl … | bash` on macOS).
    var kimiInstallCommand: String {
        readKimiBuffer(1024) { dimmy_kimi_install_command($0, $1) } ?? ""
    }

    /// Launch `kimi login` (device code) in Terminal.
    @discardableResult
    func spawnKimiLogin() -> Bool {
        dimmy_kimi_spawn_login() == 0
    }

    enum KimiPingResult {
        case ok(elapsedMs: Int32)
        case notInstalled
        case notSignedIn
        case spawnFailed
        case timeout
        case protocolError
        /// Kimi answered with an error, usually the plan's usage limit.
        case reported
        case refused
        case emptyResponse
        case unknownError
    }

    /// Content-free ping through `kimi acp`. BLOCKING — call from a
    /// background queue.
    func pingKimi() -> KimiPingResult {
        let rc = dimmy_kimi_ping()
        if rc > 0 { return .ok(elapsedMs: rc) }
        switch rc {
        case -1: return .notInstalled
        case -2: return .notSignedIn
        case -3: return .spawnFailed
        case -4: return .timeout
        case -5: return .protocolError
        case -6: return .reported
        case -7: return .refused
        case -8: return .emptyResponse
        default: return .unknownError
        }
    }

    /// The models this account's plan serves, as (id, name). Empty when the
    /// CLI is missing, signed out or failed. BLOCKING up to 30 s.
    func kimiModels() -> [(id: String, name: String)] {
        guard let json = readKimiBuffer(16384, { dimmy_kimi_models_json($0, $1) }),
              let data = json.data(using: .utf8),
              let arr = try? JSONSerialization.jsonObject(with: data) as? [[String: Any]]
        else { return [] }
        return arr.compactMap { m in
            guard let id = m["id"] as? String, !id.isEmpty else { return nil }
            return (id, (m["name"] as? String) ?? id)
        }
    }

    @discardableResult
    func recheckKimi() -> ClaudeCodeStatus {
        ClaudeCodeStatus(rawValue: dimmy_kimi_recheck()) ?? .notInstalled
    }

    struct KimiMcpStatus {
        var kimiFound = false
        /// Dimmy's entry is in ~/.kimi-code/mcp.json.
        var registered = false
        var configPath: String?
        /// Kimi Code Desktop's app bundle, when installed.
        var desktopPath: String?
        /// Seconds since Kimi last started dimmy-mcp (its own heartbeat file).
        var heartbeatAgeSecs: Int?
    }

    var kimiMcpStatus: KimiMcpStatus {
        guard let json = readKimiBuffer(8192, { dimmy_kimi_mcp_status($0, $1) }),
              let data = json.data(using: .utf8),
              let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        else { return KimiMcpStatus() }
        return KimiMcpStatus(
            kimiFound: (obj["kimi_found"] as? Bool) ?? false,
            registered: (obj["registered"] as? Bool) ?? false,
            configPath: obj["config_path"] as? String,
            desktopPath: obj["desktop_path"] as? String,
            heartbeatAgeSecs: obj["heartbeat_age_secs"] as? Int)
    }

    /// 0 done; -1 no home, -2 binary missing, -3 mcp.json invalid, -4 write failed.
    func installKimiMcp(binaryPath: String) -> Int32 {
        binaryPath.withCString { dimmy_kimi_mcp_install($0) }
    }

    @discardableResult
    func uninstallKimiMcp() -> Int32 {
        dimmy_kimi_mcp_uninstall()
    }

    private func readKimiBuffer(
        _ capacity: Int32,
        _ call: (UnsafeMutablePointer<CChar>, Int32) -> Int32
    ) -> String? {
        let buffer = UnsafeMutablePointer<CChar>.allocate(capacity: Int(capacity))
        defer { buffer.deallocate() }
        buffer[0] = 0
        let written = call(buffer, capacity)
        guard written > 0 else { return nil }
        return String(cString: buffer)
    }
}

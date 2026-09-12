import Foundation

/// Gemini CLI (Google account) FFI, mirroring the Codex block in
/// `DimmyCore.swift`.
///
/// Split into its own file rather than appended there for the same reason
/// the Confluence one is: the Codex and Claude blocks sit on a settled
/// contract, while this one carries two result codes neither of them has,
/// and it is better that nobody assumes the three behave identically.
///
/// Reuses `ClaudeCodeStatus` unchanged — the three states and their integer
/// codes are the same, pinned on the Rust side.
extension DimmyCore {

    /// Probe local Gemini CLI state. Cheap: no subprocess.
    var geminiCliStatus: ClaudeCodeStatus {
        ClaudeCodeStatus(rawValue: dimmy_gemini_cli_status()) ?? .notInstalled
    }

    var geminiCliBinaryPath: String? {
        let bufLen: Int32 = 4096
        let buffer = UnsafeMutablePointer<CChar>.allocate(capacity: Int(bufLen))
        defer { buffer.deallocate() }
        buffer[0] = 0
        let written = dimmy_gemini_cli_binary_path(buffer, bufLen)
        guard written > 0 else { return nil }
        return String(cString: buffer)
    }

    /// Launch the CLI in Terminal so the user can sign in.
    ///
    /// There is no `gemini login` subcommand: a first run with no
    /// credentials shows the auth picker itself, so this launches the CLI
    /// bare and the user chooses "Login with Google" there.
    @discardableResult
    func spawnGeminiCliLogin() -> Bool {
        return dimmy_gemini_cli_spawn_login() == 0
    }

    /// The CLI's own message from the last ping, for the `.reported` case.
    ///
    /// Empty for every other outcome: their text is redacted in Rust because
    /// it can carry a path or a fragment of the user's transcript. This one
    /// is about the REQUEST — a quota that ran out, a model that is not
    /// available — and it is the difference between "something failed" and
    /// "you have used today's requests".
    var geminiCliLastError: String {
        let bufLen: Int32 = 2048
        let buffer = UnsafeMutablePointer<CChar>.allocate(capacity: Int(bufLen))
        defer { buffer.deallocate() }
        buffer[0] = 0
        let written = dimmy_gemini_cli_last_error(buffer, bufLen)
        guard written > 0 else { return "" }
        return String(cString: buffer)
    }

    /// Outcome of a Gemini ping. The first seven cases map 1:1 onto
    /// `ClaudeCodePingResult`; the last two exist because this CLI can fail
    /// while the process exits SUCCESSFULLY, reporting the problem inside
    /// its JSON envelope. Exit status alone is not proof of success here.
    enum GeminiCliPingResult {
        case ok(elapsedMs: Int32)
        case notInstalled
        case notSignedIn
        case spawnFailed
        case timeout
        case nonZeroExit
        case invalidUtf8
        /// The CLI answered and said no. Read `geminiCliLastError`.
        case reported
        /// Exit zero, nothing usable in the response.
        case emptyResponse
        case unknownError
    }

    /// Content-free "ping" through the Gemini CLI. BLOCKING — call from a
    /// background queue.
    func pingGeminiCli() -> GeminiCliPingResult {
        let rc = dimmy_gemini_cli_ping()
        if rc > 0 { return .ok(elapsedMs: rc) }
        switch rc {
        case -1: return .notInstalled
        case -2: return .notSignedIn
        case -3: return .spawnFailed
        case -4: return .timeout
        case -5: return .nonZeroExit
        case -6: return .invalidUtf8
        case -7: return .reported
        case -8: return .emptyResponse
        default: return .unknownError
        }
    }

    @discardableResult
    func recheckGeminiCli() -> ClaudeCodeStatus {
        ClaudeCodeStatus(rawValue: dimmy_gemini_cli_recheck()) ?? .notInstalled
    }
}

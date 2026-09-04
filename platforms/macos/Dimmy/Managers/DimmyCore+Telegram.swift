import Foundation

// MARK: - DimmyCore — Telegram inbox FFI wrappers
//
// Thin Swift bridges around the Rust Telegram "Saved Messages audio
// inbox" (core/src/telegram.rs, 10 `dimmy_telegram_*` entries). Mirror of
// the Windows P/Invoke block in `Interop/DimmyNative.cs`. The Rust worker
// owns the grammers client / session / download; these wrappers stay
// pure-thin. Every entry exists regardless of the `telegram` cargo
// feature (stable C ABI) and returns -100 when the core was built
// without it.

extension DimmyCore {
    /// Master start/stop of the worker thread. Also fired implicitly by
    /// `setConfig` when `telegram_enabled` changes, so hosts rarely need
    /// to call this directly.
    func telegramSetEnabled(_ enabled: Bool) {
        guard isInitialized else { return }
        dimmy_telegram_set_enabled(enabled ? 1 : 0)
    }

    /// Request a login code for `phone` (E.164, e.g. "+39..."). Ensures
    /// the worker is started first. Returns the rc (0 queued, -1 arg /
    /// channel, -100 not compiled); the actual outcome arrives async via
    /// the `telegram_state` event.
    @discardableResult
    func telegramStartLogin(phone: String) -> Int32 {
        guard isInitialized else { return -1 }
        return phone.withCString { dimmy_telegram_start_login($0) }
    }

    /// Submit the login code (meaningful only after phase == wait_code).
    @discardableResult
    func telegramSubmitCode(_ code: String) -> Int32 {
        guard isInitialized else { return -1 }
        return code.withCString { dimmy_telegram_submit_code($0) }
    }

    /// Submit the 2FA cloud password (only after phase == wait_password).
    @discardableResult
    func telegramSubmitPassword(_ password: String) -> Int32 {
        guard isInitialized else { return -1 }
        return password.withCString { dimmy_telegram_submit_password($0) }
    }

    /// Sign the account out; emits `telegram_state {phase:"logged_out"}`.
    @discardableResult
    func telegramLogout() -> Int32 {
        guard isInitialized else { return -1 }
        return dimmy_telegram_logout()
    }

    /// Accept a pending audio: download it, then the core emits
    /// `telegram_audio {path}`.
    @discardableResult
    func telegramProcess(msgId: Int32) -> Int32 {
        guard isInitialized else { return -1 }
        return dimmy_telegram_process(msgId)
    }

    /// Reject a pending audio without processing (never re-offered).
    @discardableResult
    func telegramDismiss(msgId: Int32) -> Int32 {
        guard isInitialized else { return -1 }
        return dimmy_telegram_dismiss(msgId)
    }

    /// Mark an audio fully handled after the host transcribed it.
    @discardableResult
    func telegramMarkProcessed(msgId: Int32) -> Int32 {
        guard isInitialized else { return -1 }
        return dimmy_telegram_mark_processed(msgId)
    }

    /// Synchronous status snapshot:
    /// `{compiled, has_credentials, phase, account, pending}`. nil on
    /// read failure / not-initialized.
    func telegramStatus() -> [String: Any]? {
        guard isInitialized else { return nil }
        let bufLen: Int32 = 4096
        let buffer = UnsafeMutablePointer<CChar>.allocate(capacity: Int(bufLen))
        defer { buffer.deallocate() }
        buffer[0] = 0
        let rc = dimmy_telegram_status(buffer, bufLen)
        guard rc >= 0 else { return nil }
        let json = String(cString: buffer)
        guard let data = json.data(using: .utf8),
              let dict = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        else { return nil }
        return dict
    }

    /// Synchronous pending-list snapshot (array of
    /// `{msg_id, filename, date, size}`, sorted by date). Empty when
    /// nothing is pending or the build has no `telegram` feature.
    func telegramPendingList() -> [[String: Any]] {
        guard isInitialized else { return [] }
        let bufLen: Int32 = 1 << 16  // 64 KB
        let buffer = UnsafeMutablePointer<CChar>.allocate(capacity: Int(bufLen))
        defer { buffer.deallocate() }
        buffer[0] = 0
        let rc = dimmy_telegram_pending(buffer, bufLen)
        guard rc >= 0 else { return [] }
        let json = String(cString: buffer)
        guard let data = json.data(using: .utf8),
              let arr = try? JSONSerialization.jsonObject(with: data) as? [[String: Any]]
        else { return [] }
        return arr
    }
}

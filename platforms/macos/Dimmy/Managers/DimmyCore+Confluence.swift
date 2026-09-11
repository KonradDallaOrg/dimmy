import Foundation

/// Confluence integration FFI, mirroring the Notion block in
/// `DimmyCore+V2.swift`.
///
/// Split into its own file rather than appended there because the Notion one
/// is five short calls on a settled API, while this carries the two-endpoint
/// and two-credential handling described in `core/src/confluence.rs` — worth
/// keeping visibly separate so nobody assumes the two behave identically.
///
/// Every call returns the raw JSON envelope; the caller decodes. That is the
/// existing convention here and keeps Codable models out of the FFI layer.
extension DimmyCore {

    /// Save the Atlassian API token. Empty string clears it.
    ///
    /// The token is the only secret: site and email travel in config JSON
    /// because they are not, and putting them in the keystore would mean the
    /// UI could not show you what it is connected to.
    @discardableResult
    func confluenceSetToken(_ token: String) -> Bool {
        guard isInitialized else { return false }
        return token.withCString { ptr in
            dimmy_confluence_set_token(ptr) == 0
        }
    }

    /// True if an API token is stored, regardless of whether it still works.
    var confluenceHasToken: Bool {
        guard isInitialized else { return false }
        return dimmy_confluence_has_token() == 1
    }

    /// Check credentials against the live API.
    ///
    /// Site and email are passed IN rather than read from config because the
    /// connect sheet tests before it saves: the whole point of the button is
    /// to find out whether these values work, and persisting first would
    /// leave a broken configuration behind when they do not.
    ///
    /// An empty `token` means "use the one already stored" — what Settings
    /// does when re-testing an existing connection.
    ///
    /// Returns `{"ok":true,"site":"…","account":"…"}` or
    /// `{"ok":false,"error":"…"}`.
    func confluenceTestConnection(site: String, email: String, token: String) -> String? {
        guard isInitialized else { return nil }
        return site.withCString { sptr in
            email.withCString { eptr in
                token.withCString { tptr -> String? in
                    var buffer = [CChar](repeating: 0, count: 16384)
                    let len = buffer.withUnsafeMutableBufferPointer { ptr -> Int32 in
                        dimmy_confluence_test_connection(
                            sptr, eptr, tptr, ptr.baseAddress!, Int32(ptr.count))
                    }
                    guard len > 0 else { return nil }
                    return String(cString: buffer)
                }
            }
        }
    }

    /// Spaces the account can write to. Empty site/email fall back to config.
    ///
    /// The buffer is large on purpose: a real corporate tenant returned 530
    /// spaces, which is also why the picker needs a filter rather than a
    /// dropdown you scroll.
    func confluenceSpaces(site: String = "", email: String = "") -> String? {
        guard isInitialized else { return nil }
        return site.withCString { sptr in
            email.withCString { eptr -> String? in
                var buffer = [CChar](repeating: 0, count: 1 << 18)
                let len = buffer.withUnsafeMutableBufferPointer { ptr -> Int32 in
                    dimmy_confluence_spaces(sptr, eptr, ptr.baseAddress!, Int32(ptr.count))
                }
                guard len > 0 else { return nil }
                return String(cString: buffer)
            }
        }
    }

    /// Publish a meeting's recap.md as a new Confluence page.
    ///
    /// The core converts markdown to wiki markup (Confluence ingests no
    /// markdown) and prepends the visible AI-generated notice via
    /// `recap_for_sharing`, exactly as the Notion path does.
    ///
    /// Returns `{"ok":true,"id":"…","url":"…"}` or `{"ok":false,"error":"…"}`.
    func confluenceSendRecap(meetingDir: String) -> String? {
        guard isInitialized else { return nil }
        return meetingDir.withCString { dptr -> String? in
            var buffer = [CChar](repeating: 0, count: 16384)
            let len = buffer.withUnsafeMutableBufferPointer { ptr -> Int32 in
                dimmy_confluence_send_recap(dptr, ptr.baseAddress!, Int32(ptr.count))
            }
            guard len > 0 else { return nil }
            return String(cString: buffer)
        }
    }
}

// MARK: - Decoded shapes

/// One Confluence space offered as a destination.
struct ConfluenceSpace: Identifiable, Decodable, Hashable {
    let id: String
    let key: String
    let name: String
    let kind: String
    /// Set by the CORE for the one space it positively identified as this
    /// user's. Never derive it from `kind`: every colleague has a personal
    /// space, and labelling all of them "your space" was a real bug on the
    /// Windows side before this shipped.
    let isMine: Bool

    enum CodingKeys: String, CodingKey {
        case id, key, name, kind
        case isMine = "is_mine"
    }

    /// What the picker shows. A personal space's key is `~accountId`, which
    /// tells the reader nothing, so it is replaced by a word.
    var label: String {
        if isMine { return "\(name) (your space)" }
        if kind == "personal" { return "\(name) (personal)" }
        return "\(name) (\(key))"
    }
}

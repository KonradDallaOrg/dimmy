import Foundation

// Licensing FFI surface — Swift wrappers over the eight `dimmy_license_*`
// extern "C" functions exported by libdimmy_lib. Wire shape mirrors the
// Windows `LicenseService.cs`: same record names, same scope vocabulary.
//
// All HTTP calls block; the wrappers exposed as `async` here just wrap
// `Task.detached` so the SwiftUI views don't have to think about it.

extension DimmyCore {

    // MARK: - Types

    public struct LicenseStatus: Equatable {
        public let kind: String
        public let tier: String?
        public let daysRemaining: Int64?
        public let daysOffline: Int?
        public let error: String?
        public let cloudEnabled: Bool
        public let updatesEnabled: Bool
        public let scopes: [String]
        /// Unix epoch when an Active subscription with cancel-at-period-end
        /// will lapse. Set when the user clicked Cancel in Customer Portal;
        /// nil otherwise. UI renders "Cancels on …" subtitle.
        public let cancelsAt: Int64?
    }

    public struct LicenseOpResult {
        public let ok: Bool
        public let error: String?
        public let magicLink: String?
        public let code: String?
    }

    public struct LicenseDeviceInfo: Identifiable {
        public let deviceId: String
        public let label: String
        public let issuedAt: Int64
        public let lastSeen: Int64
        public let status: String
        public let isSelf: Bool
        public var id: String { deviceId }
    }

    public struct LicenseDevicesList {
        public let ok: Bool
        public let error: String?
        public let licenseId: String?
        public let tier: String?
        public let maxDevices: Int
        public let devices: [LicenseDeviceInfo]
    }

    public enum LicenseScope: String, CaseIterable {
        case managedStt    = "managed_stt"
        case managedLlm    = "managed_llm"
        case autoUpdate    = "auto_update"
        case historySync   = "history_sync"
        case premiumStyles = "premium_styles"

        public var display: String {
            switch self {
            case .managedStt:    return "Managed STT"
            case .managedLlm:    return "Managed LLM"
            case .autoUpdate:    return "Auto-update"
            case .historySync:   return "History sync"
            case .premiumStyles: return "Premium styles"
            }
        }

        public var subtitle: String {
            switch self {
            case .managedStt:    return "Use Dimmy-hosted speech-to-text without configuring your own provider key."
            case .managedLlm:    return "Use Dimmy-hosted post-processing LLM without configuring your own key."
            case .autoUpdate:    return "Receive in-app version updates as soon as they ship."
            case .historySync:   return "Sync your transcription history across devices (planned)."
            case .premiumStyles: return "Curated LLM styles and prompts beyond the defaults (planned)."
            }
        }
    }

    // MARK: - Status

    /// Synchronous status read — Settings views call this on appear and
    /// after every state-changing operation. Cheap (no network).
    public func licenseStatus() -> LicenseStatus {
        let buf = UnsafeMutablePointer<CChar>.allocate(capacity: 4096)
        defer { buf.deallocate() }
        buf[0] = 0
        let n = dimmy_license_status_json(buf, 4096)
        guard n > 0,
              let data = String(cString: buf).data(using: .utf8),
              let dict = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        else {
            return LicenseStatus(
                kind: "Invalid", tier: nil, daysRemaining: nil, daysOffline: nil,
                error: "FFI status returned \(n)", cloudEnabled: false, updatesEnabled: false,
                scopes: [], cancelsAt: nil)
        }
        return LicenseStatus(
            kind: dict["kind"] as? String ?? "Invalid",
            tier: dict["tier"] as? String,
            daysRemaining: (dict["days_remaining"] as? NSNumber)?.int64Value,
            daysOffline: (dict["days_offline"] as? NSNumber)?.intValue,
            error: dict["error"] as? String,
            cloudEnabled: dict["cloud_enabled"] as? Bool ?? false,
            updatesEnabled: dict["updates_enabled"] as? Bool ?? false,
            scopes: dict["scopes"] as? [String] ?? [],
            cancelsAt: (dict["cancels_at"] as? NSNumber)?.int64Value)
    }

    public func licenseHasScope(_ scope: LicenseScope) -> Bool {
        return scope.rawValue.withCString { dimmy_license_has_scope($0) == 1 }
    }

    // licenseSetServerUrl(_:) removed. The FFI is now debug-only on the
    // Rust side and the Mac Settings UI override has been deleted.
    // Release dylibs embed the server URL via DIMMY_LICENSE_SERVER_URL
    // at compile time and refuse to be re-pointed at runtime — this
    // closes the path where a staging-keyed build could be flipped to
    // hit prod (or vice-versa) by typing a URL.

    // MARK: - Operations (network-bound)

    public func licenseRequestTrial(email: String) async -> LicenseOpResult {
        await Task.detached(priority: .userInitiated) { [self] in
            let buf = UnsafeMutablePointer<CChar>.allocate(capacity: 4096)
            defer { buf.deallocate() }
            buf[0] = 0
            let n = email.withCString { dimmy_license_request_trial($0, buf, 4096) }
            return parseLicenseOp(buf: buf, n: n)
        }.value
    }

    public func licenseRedeem(code: String, deviceLabel: String?) async -> LicenseOpResult {
        await Task.detached(priority: .userInitiated) { [self] in
            let buf = UnsafeMutablePointer<CChar>.allocate(capacity: 4096)
            defer { buf.deallocate() }
            buf[0] = 0
            let n = code.withCString { codePtr in
                if let label = deviceLabel, !label.isEmpty {
                    return label.withCString { labelPtr in
                        dimmy_license_redeem(codePtr, labelPtr, buf, 4096)
                    }
                }
                return dimmy_license_redeem(codePtr, nil, buf, 4096)
            }
            return parseLicenseOp(buf: buf, n: n)
        }.value
    }

    public func licenseRefresh() async -> LicenseOpResult {
        await Task.detached(priority: .userInitiated) { [self] in
            let buf = UnsafeMutablePointer<CChar>.allocate(capacity: 4096)
            defer { buf.deallocate() }
            buf[0] = 0
            let n = dimmy_license_refresh(buf, 4096)
            return parseLicenseOp(buf: buf, n: n)
        }.value
    }

    public func licenseClear() {
        _ = dimmy_license_clear()
    }

    public func licenseDevicesList() async -> LicenseDevicesList {
        await Task.detached(priority: .userInitiated) { [self] in
            let buf = UnsafeMutablePointer<CChar>.allocate(capacity: 8192)
            defer { buf.deallocate() }
            buf[0] = 0
            let n = dimmy_license_devices_list(buf, 8192)
            guard n > 0,
                  let data = String(cString: buf).data(using: .utf8),
                  let dict = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
            else {
                return LicenseDevicesList(
                    ok: false, error: "FFI returned \(n)",
                    licenseId: nil, tier: nil, maxDevices: 0, devices: [])
            }
            let devicesArr = dict["devices"] as? [[String: Any]] ?? []
            let devices: [LicenseDeviceInfo] = devicesArr.map { d in
                LicenseDeviceInfo(
                    deviceId: d["device_id"] as? String ?? "",
                    label: d["label"] as? String ?? "",
                    issuedAt: (d["issued_at"] as? NSNumber)?.int64Value ?? 0,
                    lastSeen: (d["last_seen"] as? NSNumber)?.int64Value ?? 0,
                    status: d["status"] as? String ?? "unknown",
                    isSelf: d["is_self"] as? Bool ?? false)
            }
            return LicenseDevicesList(
                ok: dict["ok"] as? Bool ?? true,
                error: dict["error"] as? String,
                licenseId: dict["license_id"] as? String,
                tier: dict["tier"] as? String,
                maxDevices: (dict["max_devices"] as? NSNumber)?.intValue ?? 0,
                devices: devices)
        }.value
    }

    public struct LicenseUrlResult {
        public let ok: Bool
        public let url: String?
        public let error: String?
    }

    /// POST /api/checkout/create — returns Stripe Checkout URL for the
    /// chosen tier. Caller opens it via NSWorkspace.shared.open. The
    /// webhook back on the server creates the license + sends the magic
    /// link email when the user pays.
    public func licenseCheckoutUrl(tier: String) async -> LicenseUrlResult {
        await Task.detached(priority: .userInitiated) { [self] in
            let buf = UnsafeMutablePointer<CChar>.allocate(capacity: 2048)
            defer { buf.deallocate() }
            buf[0] = 0
            let n = tier.withCString { dimmy_license_checkout_url($0, buf, 2048) }
            return parseLicenseUrl(buf: buf, n: n)
        }.value
    }

    /// POST /api/billing-portal — returns Stripe Customer Portal URL.
    /// Only valid for paid licenses (those with a stripe_customer_id).
    /// Trials and source-builds get an error from the server.
    public func licenseBillingPortalUrl() async -> LicenseUrlResult {
        await Task.detached(priority: .userInitiated) { [self] in
            let buf = UnsafeMutablePointer<CChar>.allocate(capacity: 2048)
            defer { buf.deallocate() }
            buf[0] = 0
            let n = dimmy_license_billing_portal_url(buf, 2048)
            return parseLicenseUrl(buf: buf, n: n)
        }.value
    }

    private func parseLicenseUrl(buf: UnsafePointer<CChar>, n: Int32) -> LicenseUrlResult {
        guard n > 0,
              let data = String(cString: buf).data(using: .utf8),
              let dict = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        else {
            return LicenseUrlResult(ok: false, url: nil, error: "FFI returned \(n)")
        }
        return LicenseUrlResult(
            ok: dict["ok"] as? Bool ?? false,
            url: dict["url"] as? String,
            error: dict["error"] as? String)
    }

    public func licenseDeactivateDevice(deviceId: String?) async -> LicenseOpResult {
        await Task.detached(priority: .userInitiated) { [self] in
            let buf = UnsafeMutablePointer<CChar>.allocate(capacity: 1024)
            defer { buf.deallocate() }
            buf[0] = 0
            let n: Int32
            if let did = deviceId, !did.isEmpty {
                n = did.withCString { dimmy_license_device_deactivate($0, buf, 1024) }
            } else {
                n = dimmy_license_device_deactivate(nil, buf, 1024)
            }
            return parseLicenseOp(buf: buf, n: n)
        }.value
    }

    // MARK: - Private

    private func parseLicenseOp(buf: UnsafePointer<CChar>, n: Int32) -> LicenseOpResult {
        guard n > 0,
              let data = String(cString: buf).data(using: .utf8),
              let dict = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        else {
            return LicenseOpResult(ok: false, error: "FFI returned \(n)", magicLink: nil, code: nil)
        }
        return LicenseOpResult(
            ok: dict["ok"] as? Bool ?? false,
            error: dict["error"] as? String,
            magicLink: dict["magic_link"] as? String,
            code: dict["code"] as? String)
    }
}

// Notification fired when an external code path mutates license state
// (URL-scheme dispatch from AppDelegate, in particular). The license
// settings view subscribes so it can refresh status without polling.
public extension Notification.Name {
    static let dimmyLicenseChanged = Notification.Name("dimmyLicenseChanged")
    /// Posted after dimmy:// activation succeeds — Settings container
    /// observes and navigates to the License tab.
    static let dimmyOpenLicenseTab = Notification.Name("dimmyOpenLicenseTab")
}

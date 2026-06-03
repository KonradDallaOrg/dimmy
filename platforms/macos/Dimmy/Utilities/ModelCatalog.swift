import Foundation

// MARK: - ModelCatalog
//
// Single source of truth for the cloud models/providers Dimmy offers.
// The data is the JSON embedded in the Rust core (assets/model-catalog.json),
// read once via the dimmy_model_catalog_json() FFI. Every model picker on
// macOS — and the Windows app, reading the SAME embedded bytes — is built
// from this, so the lists can no longer drift apart by hand. Mirror of
// platforms/windows/Dimmy.Windows/Services/ModelCatalog.cs.
//
// Local models (Whisper / Parakeet / Gemma / Phi) are NOT here: they have
// download + bundle + feature-flag semantics and are injected separately.

struct CatalogModel: Codable, Hashable {
    let id: String
    let label: String
    let tasks: [String]
    let tier: String?
}

struct CatalogProvider: Codable, Hashable {
    let id: String
    let name: String
    let icon: String
    let llm_url: String
    let stt_url: String
    let models: [CatalogModel]
}

private struct CatalogRoot: Codable {
    let schema: Int
    let providers: [CatalogProvider]
}

enum ModelCatalog {
    /// Loaded once at first access from the embedded catalog. Empty on a
    /// decode failure (a real bug the Rust catalog unit test would catch) —
    /// left visible rather than papered over with a stale hardcoded list.
    static let providers: [CatalogProvider] = {
        guard let json = DimmyCore.shared.modelCatalogJSON(),
              let data = json.data(using: .utf8),
              let root = try? JSONDecoder().decode(CatalogRoot.self, from: data)
        else { return [] }
        return root.providers
    }()

    /// (provider, model) pairs that serve the given task, in catalog order.
    static func models(task: String) -> [(provider: CatalogProvider, model: CatalogModel)] {
        providers.flatMap { p in
            p.models.filter { $0.tasks.contains(task) }.map { (p, $0) }
        }
    }

    static func byId(_ id: String) -> CatalogProvider? {
        providers.first { $0.id.caseInsensitiveCompare(id) == .orderedSame }
    }

    /// Tier-qualified label, e.g. "Claude Opus 4.8 (top)".
    static func tierLabel(_ m: CatalogModel) -> String {
        if let t = m.tier, !t.isEmpty { return "\(m.label) (\(t))" }
        return m.label
    }
}

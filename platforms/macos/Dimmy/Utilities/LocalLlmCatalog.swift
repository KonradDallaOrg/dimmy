import Foundation

// MARK: - LocalLlmCatalog
//
// Single source of truth for the downloadable local LLM models (Gemma / Phi)
// is the Rust core's AVAILABLE_LLM_MODELS, read once via the
// dimmy_list_llm_models() FFI. This mirrors how cloud models flow through
// ModelCatalog (assets/model-catalog.json over FFI) so adding a local model
// touches only core/src/local_llm.rs — Windows already reads the same FFI list
// in SettingsWindow.PopulateLocalLlmModels.

struct LocalLlmModel: Identifiable {
    let filename: String
    let name: String
    let sizeMb: Int

    var id: String { filename }

    /// Dropdown label, e.g. "Gemma 4 E4B Q4 · 5.0 GB".
    var label: String {
        let gb = Double(sizeMb) / 1024.0
        return String(format: "%@ · %.1f GB", name, gb)
    }
}

enum LocalLlmCatalog {
    /// Loaded once on first access from the FFI. The model identity
    /// (filename / name / size) never changes at runtime — only download
    /// status does, which the picker does not display — so caching is safe.
    static let models: [LocalLlmModel] = {
        guard let raw = DimmyCore.shared.listLLMModels() else { return [] }
        return raw.compactMap { dict in
            guard let filename = dict["filename"] as? String,
                  let name = dict["name"] as? String
            else { return nil }
            let sizeMb = (dict["size_mb"] as? Int)
                ?? Int((dict["size_mb"] as? Double) ?? 0)
            return LocalLlmModel(filename: filename, name: name, sizeMb: sizeMb)
        }
    }()
}

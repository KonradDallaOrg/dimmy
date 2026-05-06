import Foundation

// MARK: - Recap model auto-pick
//
// Mirrors the Win-side `MeetingWindow.PickRecapModel`. Only the model
// NAME is overridden — the URL + key still come from the user's main
// LLM config. So if the user picked Anthropic in Settings the recap
// uses Opus; with Gemini it uses 2.5 Pro; with everything else (Groq,
// OpenAI, Together, Fireworks, OpenRouter) we keep their configured
// model — those are usually the right call already.
//
// Reading config.json directly (instead of round-tripping AppState) lets
// us call this from non-MainActor contexts like the auto-recap worker
// thread without paying for a hop back.

func pickRecapModel() -> String {
    do {
        let support = try FileManager.default.url(
            for: .applicationSupportDirectory,
            in: .userDomainMask,
            appropriateFor: nil,
            create: false
        )
        let cfgURL = support.appendingPathComponent("dimmy/config.json")
        let data = try Data(contentsOf: cfgURL)
        guard let json = try JSONSerialization.jsonObject(with: data) as? [String: Any],
              let url = json["llm_api_url"] as? String else {
            return ""
        }
        let lower = url.lowercased()
        if lower.contains("anthropic.com") { return "claude-opus-4-7" }
        if lower.contains("googleapis.com") { return "gemini-2.5-pro" }
        return ""
    } catch {
        return ""
    }
}

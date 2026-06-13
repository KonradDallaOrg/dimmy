import Foundation

/// One-shot Groq API-key validator. Mirror of
/// `platforms/windows/Dimmy.Windows/Services/GroqKeyValidator.cs`.
/// Hits Groq's `/models` endpoint with bearer auth — a 2xx response
/// proves the key is well-formed AND has list-models permissions.
enum GroqKeyValidator {
    enum Outcome {
        case ok
        case invalid(String)
    }

    private static let modelsEndpoint = URL(string: "https://api.groq.com/openai/v1/models")!
    private static let timeout: TimeInterval = 8

    static func validate(_ apiKey: String) async -> Outcome {
        let trimmed = apiKey.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return .invalid("Key is empty") }

        var req = URLRequest(url: modelsEndpoint, timeoutInterval: timeout)
        req.httpMethod = "GET"
        req.setValue("Bearer \(trimmed)", forHTTPHeaderField: "Authorization")
        req.setValue("Dimmy-Onboarding/1.0", forHTTPHeaderField: "User-Agent")

        do {
            let (_, response) = try await URLSession.shared.data(for: req)
            guard let http = response as? HTTPURLResponse else {
                return .invalid("Validation failed")
            }
            switch http.statusCode {
            case 200..<300:
                return .ok
            case 401:
                return .invalid("Invalid key")
            case 403:
                return .invalid("Key has no permissions")
            case 429:
                return .invalid("Rate limited. Try again in a moment.")
            default:
                return .invalid("Unexpected response (\(http.statusCode))")
            }
        } catch let urlErr as URLError where urlErr.code == .timedOut {
            return .invalid("Timeout. Check connection.")
        } catch {
            return .invalid("Network error. Check connection.")
        }
    }
}

import SwiftUI

struct AboutSettingsView: View {
    @State private var updateStatus: UpdateStatus = .idle
    @State private var latestVersion: String?
    @State private var downloadURL: String?

    /// Current app version — read from Rust config or Info.plist
    private var currentVersion: String {
        Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "dev"
    }

    /// Rust core version from Cargo.toml (via config)
    private var rustVersion: String {
        if let config = DimmyCore.shared.getConfig(),
           let _ = config["api_url"] {
            return "0.3.63" // TODO: expose version from Rust FFI
        }
        return "unknown"
    }

    var body: some View {
        VStack(spacing: 16) {
            Spacer()

            Image(systemName: "waveform.circle.fill")
                .font(.system(size: 48))
                .foregroundStyle(.tint)

            Text("Dimmy")
                .font(.system(size: 20, weight: .bold, design: .rounded))

            Text("Version \(currentVersion) (core \(rustVersion))")
                .font(.system(size: 12))
                .foregroundColor(.secondary)

            Text("Voice dictation that stays out of your way")
                .font(.system(size: 12))
                .foregroundColor(.secondary)

            Divider()
                .frame(width: 200)

            // Update check
            updateSection

            Divider()
                .frame(width: 200)

            HStack(spacing: 16) {
                Link("GitHub", destination: URL(string: "https://github.com/KonradDallaOrg/dimmy")!)
                    .font(.system(size: 11))
                Link("Releases", destination: URL(string: "https://github.com/KonradDallaOrg/dimmy/releases")!)
                    .font(.system(size: 11))
            }

            Text("Made with irony")
                .font(.system(size: 11))
                .foregroundColor(Color(nsColor: .tertiaryLabelColor))

            Spacer()
        }
        .frame(width: 400, height: 340)
    }

    // MARK: - Update check

    @ViewBuilder
    private var updateSection: some View {
        switch updateStatus {
        case .idle:
            Button("Check for updates") {
                checkForUpdates()
            }
            .buttonStyle(.bordered)
            .controlSize(.small)

        case .checking:
            HStack(spacing: 8) {
                ProgressView()
                    .controlSize(.small)
                Text("Checking...")
                    .font(.system(size: 12))
                    .foregroundColor(.secondary)
            }

        case .upToDate:
            HStack(spacing: 6) {
                Image(systemName: "checkmark.circle.fill")
                    .foregroundColor(.green)
                Text("You're up to date")
                    .font(.system(size: 12))
                    .foregroundColor(.secondary)
            }

        case .updateAvailable:
            VStack(spacing: 8) {
                HStack(spacing: 6) {
                    Image(systemName: "arrow.down.circle.fill")
                        .foregroundColor(.blue)
                    Text("Update available: \(latestVersion ?? "")")
                        .font(.system(size: 12, weight: .medium))
                }
                if let url = downloadURL, let dlURL = URL(string: url) {
                    Link("Download", destination: dlURL)
                        .buttonStyle(.borderedProminent)
                        .controlSize(.small)
                }
            }

        case .error(let msg):
            VStack(spacing: 4) {
                HStack(spacing: 6) {
                    Image(systemName: "exclamationmark.triangle")
                        .foregroundColor(.orange)
                    Text(msg)
                        .font(.system(size: 11))
                        .foregroundColor(.secondary)
                }
                Button("Retry") { checkForUpdates() }
                    .buttonStyle(.bordered)
                    .controlSize(.mini)
            }
        }
    }

    private func checkForUpdates() {
        updateStatus = .checking

        Task {
            do {
                let (version, url) = try await fetchLatestRelease()
                await MainActor.run {
                    latestVersion = version
                    downloadURL = url
                    if isNewer(remote: version, local: rustVersion) {
                        updateStatus = .updateAvailable
                    } else {
                        updateStatus = .upToDate
                    }
                }
            } catch {
                await MainActor.run {
                    updateStatus = .error("Could not check: \(error.localizedDescription)")
                }
            }
        }
    }

    /// Fetch latest release from GitHub API
    private func fetchLatestRelease() async throws -> (version: String, url: String) {
        let apiURL = URL(string: "https://api.github.com/repos/KonradDallaOrg/dimmy/releases/latest")!
        var request = URLRequest(url: apiURL)
        request.setValue("application/vnd.github+json", forHTTPHeaderField: "Accept")
        request.timeoutInterval = 10

        let (data, response) = try await URLSession.shared.data(for: request)
        guard let httpResponse = response as? HTTPURLResponse, httpResponse.statusCode == 200 else {
            throw URLError(.badServerResponse)
        }

        guard let json = try JSONSerialization.jsonObject(with: data) as? [String: Any],
              let tagName = json["tag_name"] as? String,
              let htmlUrl = json["html_url"] as? String
        else {
            throw URLError(.cannotParseResponse)
        }

        // Strip "v" prefix if present
        let version = tagName.hasPrefix("v") ? String(tagName.dropFirst()) : tagName
        return (version, htmlUrl)
    }

    /// Compare semver: returns true if remote > local
    private func isNewer(remote: String, local: String) -> Bool {
        let remoteParts = remote.split(separator: ".").compactMap { Int($0) }
        let localParts = local.split(separator: ".").compactMap { Int($0) }

        for i in 0..<max(remoteParts.count, localParts.count) {
            let r = i < remoteParts.count ? remoteParts[i] : 0
            let l = i < localParts.count ? localParts[i] : 0
            if r > l { return true }
            if r < l { return false }
        }
        return false
    }
}

// MARK: - Update status

private enum UpdateStatus {
    case idle
    case checking
    case upToDate
    case updateAvailable
    case error(String)
}

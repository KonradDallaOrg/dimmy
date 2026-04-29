import SwiftUI

@main
struct DimmyApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) var appDelegate
    @StateObject private var appState = AppState.shared

    var body: some Scene {
        Settings {
            // Feature-flagged Tahoe-redesign UI vs legacy. Toggle persists
            // via AppState.useTahoeSettings so QA can pin one variant.
            // The Tahoe layout is wider (1000×740) to match the design.
            Group {
                if appState.useTahoeSettings {
                    MacSettingsContainerView(appState: appState)
                } else {
                    SettingsContainerView(appState: appState)
                        .frame(minWidth: 600, minHeight: 420)
                }
            }
        }
    }
}

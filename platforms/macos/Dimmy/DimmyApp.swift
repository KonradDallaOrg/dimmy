import SwiftUI

@main
struct DimmyApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) var appDelegate

    var body: some Scene {
        Settings {
            SettingsContainerView(appState: AppState.shared)
                .frame(minWidth: 600, minHeight: 420)
        }
    }
}

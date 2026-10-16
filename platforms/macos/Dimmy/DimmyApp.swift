import SwiftUI

/// `Dimmy --setup <command>` runs the terminal setup tool an AI assistant
/// uses to install and configure Dimmy (docs/dev/agent-setup.md), then exits
/// before any window, menu-bar item or audio starts. Windows reaches the same
/// core entry through `dimmy-setup.exe`.
@main
enum DimmyMain {
    static func main() {
        let args = CommandLine.arguments
        if args.count > 1 && args[1] == "--setup" {
            exit(runSetup(["dimmy-setup"] + args.dropFirst(2)))
        }
        DimmyApp.main()
    }

    private static func runSetup(_ argv: [String]) -> Int32 {
        precondition(!argv.isEmpty, "argv[0] is the program name")
        let cStrings: [UnsafeMutablePointer<CChar>] = argv.map { strdup($0)! }
        defer { cStrings.forEach { free($0) } }
        let pointers: [UnsafePointer<CChar>] = cStrings.map { UnsafePointer($0) }
        return pointers.withUnsafeBufferPointer { buf in
            dimmy_setup_cli(Int32(argv.count), buf.baseAddress!)
        }
    }
}

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

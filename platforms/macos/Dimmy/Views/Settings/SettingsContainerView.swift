import SwiftUI

enum SettingsTab: String, CaseIterable, Identifiable {
    case general = "General"
    case models = "Models"
    case shortcut = "Shortcut"
    case output = "Output"
    case pill = "Overlay"
    case history = "History"
    case permissions = "Permissions"
    case stats = "Stats"
    case debug = "Debug"
    case diagnostics = "Diagnostics"
    case about = "About"

    var id: String { rawValue }

    var icon: String {
        switch self {
        case .general: return "gear"
        case .models: return "cpu"
        case .shortcut: return "command.square"
        case .output: return "doc.text"
        case .pill: return "capsule"
        case .history: return "clock.arrow.circlepath"
        case .permissions: return "lock.shield"
        case .stats: return "chart.bar"
        case .debug: return "ant"
        case .diagnostics: return "stethoscope"
        case .about: return "info.circle"
        }
    }
}

struct SettingsContainerView: View {
    @ObservedObject var appState: AppState
    @State private var selectedTab: SettingsTab = .general

    /// Tabs visible based on the Advanced toggle state
    private var visibleTabs: [SettingsTab] {
        SettingsTab.allCases.filter { tab in
            switch tab {
            case .debug:
                return appState.showAdvanced
            case .diagnostics:
                return appState.showAdvanced
            default:
                return true
            }
        }
    }

    var body: some View {
        NavigationSplitView {
            VStack(spacing: 0) {
                List(visibleTabs, selection: $selectedTab) { tab in
                    Label(tab.rawValue, systemImage: tab.icon)
                        .tag(tab)
                }
                .listStyle(.sidebar)

                Divider()

                Toggle("Advanced", isOn: $appState.showAdvanced)
                    .toggleStyle(.switch)
                    .padding(.horizontal, 16)
                    .padding(.vertical, 10)
            }
            .navigationSplitViewColumnWidth(min: 160, ideal: 180, max: 200)
        } detail: {
            ScrollView {
                detailView
                    .frame(maxWidth: 480)
                    .padding(.vertical, 8)
            }
            .frame(maxWidth: .infinity)
        }
    }

    @ViewBuilder
    private var detailView: some View {
        switch selectedTab {
        case .general:
            GeneralSettingsView(appState: appState)
        case .models:
            Form {
                Section("Speech Recognition Models") {
                    ModelSettingsView(appState: appState)
                }
            }
            .formStyle(.grouped)
        case .shortcut:
            ShortcutSettingsView(appState: appState)
        case .output:
            OutputSettingsView(appState: appState)
        case .pill:
            OverlaySettingsView(appState: appState)
        case .history:
            HistorySettingsView(appState: appState)
        case .permissions:
            PermissionsSettingsView(appState: appState)
        case .stats:
            StatsSettingsView(appState: appState)
        case .debug:
            DebugSettingsView(appState: appState)
        case .diagnostics:
            DiagnosticsSettingsView(appState: appState)
        case .about:
            AboutSettingsView()
        }
    }
}

import AppKit
import SwiftUI

@main
struct VibeFactoryApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    @State private var settings = AppSettings()
    @State private var recents = RecentProjects()
    private let registry = SessionRegistry.shared

    var body: some Scene {
        Window(settings.t("welcome_title"), id: WindowID.welcome) {
            WelcomeView()
                .environment(settings)
                .environment(recents)
                .preferredColorScheme(settings.appearance.colorScheme)
        }
        .windowResizability(.contentSize)
        .defaultPosition(.center)
        .commands {
            AppCommands(settings: settings, recents: recents)
        }

        WindowGroup(for: ProjectRef.self) { $ref in
            ProjectWindow(ref: ref)
                .environment(settings)
                .environment(recents)
                .environment(registry)
                .preferredColorScheme(settings.appearance.colorScheme)
        }
        .windowStyle(.titleBar)
        .windowToolbarStyle(.unified)
        .defaultSize(width: 1180, height: 760)

        Settings {
            SettingsView()
                .environment(settings)
                .preferredColorScheme(settings.appearance.colorScheme)
        }

        MenuBarExtra {
            MenuBarView()
                .environment(settings)
                .environment(registry)
        } label: {
            MenuBarLabel(registry: registry)
        }
    }
}

enum WindowID {
    static let welcome = "welcome"
}

/// Stops the `vibe serve` children when the app quits: window `onDisappear`
/// does not run reliably on ⌘Q.
final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationDidFinishLaunching(_ notification: Notification) {
        MainActor.assumeIsolated { Notifications.install() }
    }

    func applicationWillTerminate(_ notification: Notification) {
        MainActor.assumeIsolated {
            SessionRegistry.shared.stopAllForTermination()
        }
    }

    /// Closing the last project window leaves the app in the menu bar.
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { false }
}

/// The menu bar icon: running tasks and pending approvals of every open
/// project. Always on screen, it also opens the windows asked from outside a
/// view (notification clicks).
struct MenuBarLabel: View {
    let registry: SessionRegistry
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        let running = registry.runningCount
        let approvals = registry.approvalCount
        Group {
            if running == 0 && approvals == 0 {
                Image(systemName: "hammer")
            } else {
                Text(approvals > 0 ? "⚒︎\(running) ⏸\(approvals)" : "⚒︎\(running)")
            }
        }
        .onChange(of: registry.windowRequest) { _, ref in
            guard let ref else { return }
            registry.windowRequest = nil
            openWindow(value: ref)
            NSApplication.shared.activate(ignoringOtherApps: true)
        }
    }
}

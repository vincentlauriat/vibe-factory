import SwiftUI
import VibeAPI

/// The menu bar item: running tasks and pending approvals of every open project.
struct MenuBarView: View {
    @Environment(AppSettings.self) private var settings
    @Environment(SessionRegistry.self) private var registry
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        let sessions = registry.sortedSessions
        Text(settings.t("menubar_summary", "\(registry.runningCount)", "\(registry.approvalCount)"))
        if sessions.isEmpty {
            Text(settings.t("menubar_no_project"))
        }
        ForEach(sessions) { session in
            Divider()
            Button(session.ref.displayName) { openWindow(value: session.ref) }
            ForEach(session.approvalRows) { row in
                Button("⏸ \(row.label) \(row.task.title) — \(row.run?.pendingApproval?.display ?? "")") {
                    openWindow(value: session.ref)
                }
            }
            ForEach(session.runningRows) { row in
                Button("▶ \(row.label) \(row.task.title) — \(row.run?.currentPhase.display ?? "")") {
                    openWindow(value: session.ref)
                }
            }
        }
        Divider()
        Button(settings.t("menu_welcome")) {
            openWindow(id: WindowID.welcome)
            NSApplication.shared.activate(ignoringOtherApps: true)
        }
        SettingsLink { Text(settings.t("settings_title")) }
        Divider()
        Button(settings.t("quit")) { NSApplication.shared.terminate(nil) }
            .keyboardShortcut("q")
    }
}

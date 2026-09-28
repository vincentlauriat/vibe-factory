import SwiftUI

/// The focused project window, for the menu commands.
struct FocusedProjectKey: FocusedValueKey {
    typealias Value = ProjectViewModel
}

extension FocusedValues {
    var project: ProjectViewModel? {
        get { self[FocusedProjectKey.self] }
        set { self[FocusedProjectKey.self] = newValue }
    }
}

/// File and Task menus: Open Project ⌘O, New Task ⌘N, Run ⌘R, Resume ⇧⌘R,
/// Cancel ⌘., Approve ⌥⌘A, Reject ⌥⌘J.
struct AppCommands: Commands {
    let settings: AppSettings
    let recents: RecentProjects
    @FocusedValue(\.project) private var project
    @Environment(\.openWindow) private var openWindow

    var body: some Commands {
        CommandGroup(replacing: .newItem) {
            Button(settings.t("menu_open_project")) {
                if let ref = WelcomeViewModel().chooseFolder(prompt: settings.t("open")) {
                    recents.add(ref)
                    openWindow(value: ref)
                }
            }
            .keyboardShortcut("o")
            Button(settings.t("menu_welcome")) { openWindow(id: WindowID.welcome) }
                .keyboardShortcut("0", modifiers: [.command, .shift])
        }
        CommandMenu(settings.t("menu_task")) {
            Button(settings.t("action_new_task")) { project?.showingNewTask = true }
                .keyboardShortcut("n")
                .disabled(!(project?.isConnected ?? false))
            Divider()
            Button(settings.t("action_run")) { project?.run() }
                .keyboardShortcut("r")
                .disabled(!(project?.canRun ?? false))
            Button(settings.t("action_resume")) { project?.resume() }
                .keyboardShortcut("r", modifiers: [.command, .shift])
                .disabled(!(project?.canResume ?? false))
            Button(settings.t("action_cancel")) { project?.cancel() }
                .keyboardShortcut(".")
                .disabled(!(project?.canCancel ?? false))
            Divider()
            Button(settings.t("action_approve")) { project?.decision = .approve }
                .keyboardShortcut("a", modifiers: [.command, .option])
                .disabled(!(project?.canDecide ?? false))
            Button(settings.t("action_reject")) { project?.decision = .reject }
                .keyboardShortcut("j", modifiers: [.command, .option])
                .disabled(!(project?.canDecide ?? false))
        }
    }
}

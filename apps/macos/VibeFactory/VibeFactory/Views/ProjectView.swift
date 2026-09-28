import SwiftUI
import VibeAPI

/// Sidebar (board by status, Activity, History, Evaluations), content list, detail.
struct ProjectView: View {
    @Bindable var model: ProjectViewModel
    @Environment(AppSettings.self) private var settings

    var body: some View {
        NavigationSplitView {
            SidebarView(model: model)
                .navigationSplitViewColumnWidth(min: 190, ideal: 210, max: 260)
        } content: {
            if model.sidebar == .history {
                // The History table needs its ten columns.
                content.navigationSplitViewColumnWidth(min: 520, ideal: 760, max: 1_200)
            } else {
                content.navigationSplitViewColumnWidth(min: 260, ideal: 340, max: 520)
            }
        } detail: {
            detail
        }
        .toolbar { toolbar }
        .focusedSceneValue(\.project, model)
        .onAppear { model.rebuildDetail() } // the client is new after a reconnection
        .onChange(of: model.session.rows) { _, _ in model.boardChanged() }
        .onChange(of: model.session.requestedTask) { _, _ in model.showRequestedTask() }
        .sheet(isPresented: $model.showingNewTask) {
            NewTaskSheet { title, description in
                Task { await model.createTask(title: title, description: description) }
            }
        }
        .sheet(item: $model.decision) { decision in
            if let detail = model.detail {
                DecisionSheet(decision: decision, gate: detail.pendingGate?.display ?? "") { text in
                    Task {
                        switch decision {
                        case .approve: await detail.approve(comment: text)
                        case .reject: await detail.reject(reason: text)
                        }
                    }
                }
            }
        }
        .alert(settings.t("error_title"), isPresented: Binding(
            get: { model.errorMessage != nil },
            set: { if !$0 { model.errorMessage = nil } }
        )) {
            Button(settings.t("ok")) { model.errorMessage = nil }
        } message: {
            Text(model.errorMessage ?? "")
        }
    }

    @ViewBuilder
    private var content: some View {
        switch model.sidebar {
        case .status(let column):
            TaskListView(model: model, column: column)
        case nil:
            TaskListView(model: model, column: .backlog)
        case .activity:
            ActivityFeedView(model: model)
        case .history:
            HistoryView(model: model.history, session: model.session)
        case .evaluations:
            EvaluationsListView(model: model.evaluations) {
                await model.evaluations.load(model.session.client)
            }
        }
    }

    @ViewBuilder
    private var detail: some View {
        switch model.sidebar {
        case .evaluations:
            EvaluationTableView(suite: model.evaluations.suite, enabled: model.evaluations.response?.enabled ?? true)
        case .history:
            HistoryDetailView(history: model.history.selected)
        default:
            if let detail = model.detail {
                TaskDetailView(model: detail, project: model)
                    .id(detail.taskId)
            } else {
                ContentUnavailableView(settings.t("select_task"), systemImage: "square.stack.3d.up",
                                       description: Text(settings.t("select_task_desc")))
            }
        }
    }

    @ToolbarContentBuilder
    private var toolbar: some ToolbarContent {
        ToolbarItemGroup(placement: .primaryAction) {
            Button { model.showingNewTask = true } label: {
                Label(settings.t("action_new_task"), systemImage: "plus")
            }
            .help(settings.t("action_new_task") + " (⌘N)")
            Button { model.run() } label: {
                Label(settings.t("action_run"), systemImage: "play.fill")
            }
            .disabled(!model.canRun)
            .help(settings.t("action_run") + " (⌘R)")
            Button { model.resume() } label: {
                Label(settings.t("action_resume"), systemImage: "arrow.clockwise")
            }
            .disabled(!model.canResume)
            .help(settings.t("action_resume") + " (⇧⌘R)")
            Button { model.cancel() } label: {
                Label(settings.t("action_cancel"), systemImage: "stop.fill")
            }
            .disabled(!model.canCancel)
            .help(settings.t("action_cancel") + " (⌘.)")
        }
    }
}

import Foundation
import Observation
import VibeAPI

/// A project window: sidebar selection, selected task, sheets, and the
/// actions the menu commands reach through `focusedSceneValue`.
@Observable
@MainActor
final class ProjectViewModel {
    /// The approval sheet: approve with an optional comment, or reject with a reason.
    enum Decision: Identifiable {
        case approve, reject
        var id: Self { self }
    }

    let session: ProjectSession
    var sidebar: SidebarItem? = .status(.backlog)
    var selectedTaskId: String? {
        didSet { if selectedTaskId != oldValue { rebuildDetail() } }
    }
    private(set) var detail: TaskDetailViewModel?
    var showingNewTask = false
    var decision: Decision?
    var errorMessage: String?
    let evaluations = EvaluationsViewModel()
    let activity = ActivityViewModel()
    let history = HistoryViewModel()

    @ObservationIgnored private let translate: (String) -> String

    init(session: ProjectSession, translate: @escaping (String) -> String) {
        self.session = session
        self.translate = translate
    }

    var selectedRow: TaskRow? { selectedTaskId.flatMap(session.row) }

    /// The detail needs a client: rebuilt when the selection or the connection changes.
    func rebuildDetail() {
        detail?.stop()
        detail = nil
        guard let id = selectedTaskId, let client = session.client else { return }
        let model = TaskDetailViewModel(taskId: id, client: client, session: session, translate: translate)
        detail = model
        model.start()
    }

    /// The board was polled: forward the selected row to the detail.
    func boardChanged() {
        if let row = selectedRow { detail?.boardRowChanged(row) }
        showRequestedTask()
    }

    /// A notification asked for a task: select its column and the task.
    func showRequestedTask() {
        guard let id = session.requestedTask, let row = session.row(id) else { return }
        session.requestedTask = nil
        sidebar = .status(BoardColumn(row.task.status))
        selectedTaskId = id
    }

    func createTask(title: String, description: String) async {
        guard let client = session.client else { return }
        do {
            let row = try await client.createTask(title: title, description: description)
            await session.refresh()
            sidebar = .status(BoardColumn(row.task.status))
            selectedTaskId = row.id
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    // MARK: Commands

    var isConnected: Bool { session.state == .connected }
    var canRun: Bool { detail?.canRun ?? false }
    var canResume: Bool { detail?.canResume ?? false }
    var canCancel: Bool { detail?.canCancel ?? false }
    var canDecide: Bool { detail?.pendingGate != nil && !(detail?.acting ?? true) }

    func run() { Task { await detail?.run(resume: false) } }
    func resume() { Task { await detail?.run(resume: true) } }
    func cancel() { Task { await detail?.cancel() } }
}

/// The Evaluations view: summaries found under `vibe serve --evals`.
@Observable
@MainActor
final class EvaluationsViewModel {
    private(set) var response: EvalsResponse?
    private(set) var loading = false
    var selectedSuite: String?
    var errorMessage: String?

    var suite: EvalSuite? {
        response?.suites.first { $0.id == selectedSuite }
    }

    func load(_ client: VibeClient?) async {
        guard let client else { return }
        loading = true
        defer { loading = false }
        do {
            let response = try await client.evals()
            self.response = response
            if selectedSuite == nil { selectedSuite = response.suites.first?.id }
        } catch {
            errorMessage = error.localizedDescription
        }
    }
}

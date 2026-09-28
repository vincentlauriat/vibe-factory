import Foundation
import Observation
import VibeAPI

/// The History view: finished tasks (failed and cancelled ones on demand)
/// and the detail of the selected one, reloaded when a run finishes.
@Observable
@MainActor
final class HistoryViewModel {
    private(set) var histories: [TaskHistory] = []
    private(set) var loaded = false
    private(set) var loading = false
    /// Also failed and cancelled tasks.
    var showAll = false
    var selectedId: String? {
        didSet { if selectedId != oldValue { loadDetail() } }
    }
    /// `GET /api/history/{task}` of the selection; the row stands in until it arrives.
    private(set) var detail: TaskHistory?
    var errorMessage: String?

    /// The client of the last load, for the detail.
    @ObservationIgnored private var client: VibeClient?
    @ObservationIgnored private var detailTask: Task<Void, Never>?

    var rows: [HistoryRowText] { histories.map(HistoryRowText.init) }

    /// The detail to show: the loaded one, else the list's row.
    var selected: TaskHistory? {
        guard let selectedId else { return nil }
        if let detail, detail.id == selectedId { return detail }
        return histories.first { $0.id == selectedId }
    }

    /// Reload the list (and the selection's detail); the view calls it on
    /// appear, on the toggle and when a run finishes, cancelled on disappear.
    func load(_ client: VibeClient?) async {
        guard let client else { return }
        self.client = client
        loading = true
        defer { loading = false }
        do {
            histories = try await client.history(all: showAll)
            loaded = true
            loadDetail()
        } catch is CancellationError {
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func stop() {
        detailTask?.cancel()
    }

    private func loadDetail() {
        detailTask?.cancel()
        guard let selectedId, let client else {
            detail = nil
            return
        }
        detailTask = Task { [weak self] in
            do {
                let detail = try await client.history(task: selectedId)
                guard self?.selectedId == selectedId else { return }
                self?.detail = detail
            } catch is CancellationError {
            } catch {
                self?.errorMessage = error.localizedDescription
            }
        }
    }
}

/// The project-wide Activity feed of a window: which groups and task it
/// shows, and whether it is paused.
@Observable
@MainActor
final class ActivityViewModel {
    var filter = ActivityFilter()
    /// Paused at this entry id: later entries are counted, not shown, and
    /// the feed stops following the bottom.
    private(set) var pausedAt: Int?

    var paused: Bool { pausedAt != nil }

    func togglePause(_ feed: ProjectFeed) {
        pausedAt = pausedAt == nil ? (feed.entries.last?.id ?? -1) : nil
    }

    func toggle(_ group: EventGroup) {
        if filter.groups.contains(group) { filter.groups.remove(group) } else { filter.groups.insert(group) }
    }

    /// Entries the filter keeps, up to the pause.
    func visible(_ feed: ProjectFeed) -> [ProjectFeed.Entry] {
        feed.entries.filter { entry in
            (pausedAt.map { entry.id <= $0 } ?? true) && filter.keeps(task: entry.task, type: entry.type)
        }
    }

    /// Entries kept by the filter that arrived after the pause.
    func held(_ feed: ProjectFeed) -> Int {
        guard let pausedAt else { return 0 }
        return feed.entries.reversed().prefix { $0.id > pausedAt }
            .filter { filter.keeps(task: $0.task, type: $0.type) }.count
    }
}

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

    /// What the last `visible` call kept, so that the next one only looks at
    /// the entries the feed added since. Not observed: it changes during a
    /// view update.
    @ObservationIgnored private var cache: VisibleCache?

    private struct VisibleCache {
        let filter: ActivityFilter
        let pausedAt: Int?
        /// Id of the last feed entry looked at.
        var lastId: Int
        var entries: [ProjectFeed.Entry]
    }

    /// Entries the filter keeps, up to the pause. The feed grows by one
    /// entry per event and its ids only increase, so the whole buffer is
    /// filtered again only when the filter or the pause changes.
    func visible(_ feed: ProjectFeed) -> [ProjectFeed.Entry] {
        let entries = feed.entries
        var cache: VisibleCache
        if let kept = self.cache, kept.filter == filter, kept.pausedAt == pausedAt {
            cache = kept
            // The feed drops its oldest entries past its limit (all of them
            // when the stream reseeds it): drop them from the result too.
            if let first = entries.first?.id, let keptFirst = cache.entries.first?.id, keptFirst < first {
                cache.entries.removeFirst(Self.count(upTo: first, in: cache.entries))
            } else if entries.isEmpty {
                cache.entries.removeAll()
            }
        } else {
            cache = VisibleCache(filter: filter, pausedAt: pausedAt, lastId: -1, entries: [])
        }
        let fresh = entries[Self.count(upTo: cache.lastId + 1, in: entries)...]
        for entry in fresh where keeps(entry) {
            cache.entries.append(entry)
        }
        if let last = entries.last?.id { cache.lastId = last }
        self.cache = cache
        return cache.entries
    }

    private func keeps(_ entry: ProjectFeed.Entry) -> Bool {
        (pausedAt.map { entry.id <= $0 } ?? true) && filter.keeps(task: entry.task, type: entry.type)
    }

    /// Number of entries whose id is below `id`; `entries` are ordered by id.
    private static func count(upTo id: Int, in entries: [ProjectFeed.Entry]) -> Int {
        var low = 0
        var high = entries.count
        while low < high {
            let mid = (low + high) / 2
            if entries[mid].id < id { low = mid + 1 } else { high = mid }
        }
        return low
    }

    /// Entries kept by the filter that arrived after the pause.
    func held(_ feed: ProjectFeed) -> Int {
        guard let pausedAt else { return 0 }
        return feed.entries.reversed().prefix { $0.id > pausedAt }
            .filter { filter.keeps(task: $0.task, type: $0.type) }.count
    }
}

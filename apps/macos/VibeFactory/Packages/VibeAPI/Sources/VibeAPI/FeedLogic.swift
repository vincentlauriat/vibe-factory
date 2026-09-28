import Foundation

// The pure parts of the app's view models, here to be tested: the
// `(run, seq)` check of a task's events, the run a Trace tab shows, and the
// incremental filter of the Activity feed.

/// Run and `seq` of the last logged envelope delivered. A task's backlog
/// and its live events overlap, and `seq` restarts at 1 with each run: an
/// envelope of the same run with a `seq` not greater was delivered already.
public struct RunPosition: Hashable, Sendable {
    public private(set) var run: String?
    public private(set) var seq: UInt64

    public init(seq: UInt64 = 0) {
        self.seq = seq
    }

    public mutating func reset() {
        run = nil
        seq = 0
    }

    /// Whether to deliver the envelope; records its position.
    public mutating func accept(_ envelope: Envelope) -> Bool {
        guard let next = envelope.seq else { return true } // ephemeral, or logged before 0.3
        let envelopeRun = envelope.event.runId
        if envelopeRun == run, next <= seq { return false }
        run = envelopeRun
        seq = next
        return true
    }
}

/// Which run the Trace tab shows after reloading the runs of its task.
public enum RunSelection {
    /// The picked run is kept, unless it was the last one (or none was
    /// picked): then the newest run is followed. A picked run that is gone
    /// also falls back to the newest.
    /// - Parameters:
    ///   - runs: the run ids just loaded, oldest first.
    ///   - previous: the run ids shown before, oldest first.
    ///   - selected: the run shown before.
    public static func pick(_ runs: [String], previous: [String], selected: String?) -> String? {
        let keep = previous.last == selected ? nil : selected
        return runs.first { $0 == keep } ?? runs.last
    }
}

/// What the Activity feed needs of an entry to filter it.
public protocol FeedEntry {
    /// Increases with each entry the feed adds, never reused.
    var id: Int { get }
    /// Task id.
    var task: String { get }
    /// Event type.
    var type: String { get }
}

/// The entries an `ActivityFilter` keeps, up to a pause, computed
/// incrementally: the feed grows by one entry per event, drops its oldest
/// ones past its limit (all of them when it is seeded again) and its ids only
/// increase, so only the entries added since the last call are filtered.
/// Everything is filtered again when the filter or the pause changes.
public struct VisibleEntries<Entry: FeedEntry> {
    private var filter: ActivityFilter?
    private var pausedAt: Int?
    /// Id of the last feed entry looked at.
    private var lastId = -1
    private var kept: [Entry] = []

    public init() {}

    /// - Parameter pausedAt: entries after this id are not shown.
    public mutating func visible(_ entries: [Entry], filter: ActivityFilter, pausedAt: Int?) -> [Entry] {
        if self.filter == filter, self.pausedAt == pausedAt {
            // Drop the heads the feed dropped.
            if let first = entries.first?.id, let keptFirst = kept.first?.id, keptFirst < first {
                kept.removeFirst(Self.count(upTo: first, in: kept))
            } else if entries.isEmpty {
                kept.removeAll()
            }
        } else {
            self.filter = filter
            self.pausedAt = pausedAt
            lastId = -1
            kept = []
        }
        let fresh = entries[Self.count(upTo: lastId + 1, in: entries)...]
        for entry in fresh where Self.keeps(entry, filter: filter, pausedAt: pausedAt) {
            kept.append(entry)
        }
        if let last = entries.last?.id { lastId = last }
        return kept
    }

    /// Entries kept by the filter that arrived after the pause.
    public static func held(_ entries: [Entry], filter: ActivityFilter, pausedAt: Int?) -> Int {
        guard let pausedAt else { return 0 }
        return entries.reversed().prefix { $0.id > pausedAt }
            .filter { filter.keeps(task: $0.task, type: $0.type) }.count
    }

    /// Whether `visible` shows an entry, on its own.
    public static func keeps(_ entry: Entry, filter: ActivityFilter, pausedAt: Int?) -> Bool {
        (pausedAt.map { entry.id <= $0 } ?? true) && filter.keeps(task: entry.task, type: entry.type)
    }

    /// Number of entries whose id is below `id`; `entries` are ordered by id.
    private static func count(upTo id: Int, in entries: [Entry]) -> Int {
        var low = 0
        var high = entries.count
        while low < high {
            let mid = (low + high) / 2
            if entries[mid].id < id { low = mid + 1 } else { high = mid }
        }
        return low
    }
}

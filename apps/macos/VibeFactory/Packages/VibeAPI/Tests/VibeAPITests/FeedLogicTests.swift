import XCTest
@testable import VibeAPI

/// The logic the app's view models take from the package: the `(run, seq)`
/// check, the run of the Trace tab, the incremental Activity filter.
final class FeedLogicTests: XCTestCase {
    private func envelope(_ run: String, _ seq: UInt64?) -> Envelope {
        Envelope(seq: seq, at: Date(timeIntervalSince1970: 0), event: .paused(run: run, reason: "\(seq ?? 0)"))
    }

    // MARK: RunPosition

    /// The task detail loads the backlog while the live events it received
    /// meanwhile are held: the overlap is delivered once.
    func testBacklogThenHeldLiveEventsAreDeliveredOnce() {
        var position = RunPosition()
        let backlog = (1...5).map { envelope("r1", $0) }
        let held = (4...7).map { envelope("r1", $0) }
        let delivered = (backlog + held).filter { position.accept($0) }
        XCTAssertEqual(delivered.map(\.seq), [1, 2, 3, 4, 5, 6, 7])
        XCTAssertEqual(position.run, "r1")
        XCTAssertEqual(position.seq, 7)
    }

    func testANewRunRestartsAtOneAndEphemeralEventsAlwaysPass() {
        var position = RunPosition()
        XCTAssertTrue(position.accept(envelope("r1", 3)))
        XCTAssertFalse(position.accept(envelope("r1", 3)))
        // `seq` restarts with the next run.
        XCTAssertTrue(position.accept(envelope("r2", 1)))
        XCTAssertFalse(position.accept(envelope("r2", 1)))
        // No seq: an `agent_delta`, or a line logged before 0.3.
        XCTAssertTrue(position.accept(envelope("r2", nil)))
        XCTAssertTrue(position.accept(envelope("r2", nil)))
        XCTAssertEqual(position.seq, 1)
        // A resumed stream starts from its `after`, of an unknown run.
        position = RunPosition(seq: 4)
        XCTAssertTrue(position.accept(envelope("r2", 2)))
        position.reset()
        XCTAssertNil(position.run)
        XCTAssertEqual(position.seq, 0)
    }

    // MARK: RunSelection

    func testTheTraceFollowsTheNewestRunUnlessAnOlderOneIsPicked() {
        // First load: the newest.
        XCTAssertEqual(RunSelection.pick(["a", "b"], previous: [], selected: nil), "b")
        // On the last run: a new run is followed.
        XCTAssertEqual(RunSelection.pick(["a", "b", "c"], previous: ["a", "b"], selected: "b"), "c")
        // An older run picked by hand is kept.
        XCTAssertEqual(RunSelection.pick(["a", "b", "c"], previous: ["a", "b"], selected: "a"), "a")
        // A picked run that is gone: the newest.
        XCTAssertEqual(RunSelection.pick(["b", "c"], previous: ["a", "b"], selected: "a"), "c")
        // No runs.
        XCTAssertNil(RunSelection.pick([], previous: ["a"], selected: "a"))
        XCTAssertNil(RunSelection.pick([], previous: [], selected: nil))
    }

    // MARK: VisibleEntries

    private struct Entry: FeedEntry, Equatable {
        let id: Int
        let task: String
        let type: String
    }

    /// The feed as `ProjectFeed` keeps it: ids never reused (also for events
    /// it does not show), at most `limit` entries, a seed replaces them all.
    private struct Feed {
        let limit: Int
        var entries: [Entry] = []
        var nextId = 0

        mutating func append(task: String, type: String, shown: Bool = true) {
            defer { nextId += 1 }
            guard shown else { return }
            entries.append(Entry(id: nextId, task: task, type: type))
            if entries.count > limit { entries.removeFirst(entries.count - limit) }
        }

        mutating func seed(_ count: Int, task: String, type: String) {
            entries = (0..<count).map { Entry(id: nextId + $0, task: task, type: type) }.suffix(limit)
            nextId += count
        }
    }

    /// SplitMix64: a seeded generator, for a sequence that repeats.
    private struct SplitMix64: RandomNumberGenerator {
        var state: UInt64
        mutating func next() -> UInt64 {
            state &+= 0x9E37_79B9_7F4A_7C15
            var z = state
            z = (z ^ (z >> 30)) &* 0xBF58_476D_1CE4_E5B9
            z = (z ^ (z >> 27)) &* 0x94D0_49BB_1331_11EB
            return z ^ (z >> 31)
        }
    }

    /// Random appends, trims, seeds, filter and pause changes: after every
    /// step the incremental result is the feed filtered from scratch.
    func testIncrementalVisibleEntriesEqualAFilterFromScratch() {
        let tasks = ["t1", "t2", "t3"]
        let types = EventGroup.allCases.flatMap(\.types) + ["hologram"]
        for seed in UInt64(1)...8 {
            var rng = SplitMix64(state: seed)
            var feed = Feed(limit: 12)
            var cache = VisibleEntries<Entry>()
            var filter = ActivityFilter()
            var pausedAt: Int?
            for step in 0..<400 {
                switch Int.random(in: 0..<100, using: &rng) {
                case 0..<60:
                    feed.append(task: tasks.randomElement(using: &rng)!, type: types.randomElement(using: &rng)!,
                                shown: Int.random(in: 0..<5, using: &rng) > 0)
                case 60..<63:
                    feed.seed(Int.random(in: 0..<20, using: &rng), task: tasks.randomElement(using: &rng)!,
                              type: types.randomElement(using: &rng)!)
                case 63..<75:
                    let group = EventGroup.allCases.randomElement(using: &rng)!
                    if filter.groups.contains(group) { filter.groups.remove(group) } else { filter.groups.insert(group) }
                case 75..<82:
                    filter.task = Bool.random(using: &rng) ? nil : tasks.randomElement(using: &rng)
                case 82..<88:
                    pausedAt = pausedAt == nil ? (feed.entries.last?.id ?? -1) : nil
                default:
                    break // no change: the cached result is returned again
                }
                let expected = feed.entries.filter { VisibleEntries.keeps($0, filter: filter, pausedAt: pausedAt) }
                XCTAssertEqual(cache.visible(feed.entries, filter: filter, pausedAt: pausedAt), expected,
                               "seed \(seed), step \(step)")
            }
        }
    }

    func testHeldCountsTheKeptEntriesAfterThePause() {
        let entries = [Entry(id: 1, task: "a", type: "log"), Entry(id: 2, task: "a", type: "paused"),
                       Entry(id: 4, task: "b", type: "log"), Entry(id: 5, task: "a", type: "committed")]
        XCTAssertEqual(VisibleEntries.held(entries, filter: ActivityFilter(), pausedAt: nil), 0)
        XCTAssertEqual(VisibleEntries.held(entries, filter: ActivityFilter(), pausedAt: 2), 2)
        XCTAssertEqual(VisibleEntries.held(entries, filter: ActivityFilter(task: "a"), pausedAt: 2), 1)
        XCTAssertEqual(VisibleEntries.held(entries, filter: ActivityFilter(groups: [.logs]), pausedAt: -1), 2)
    }
}

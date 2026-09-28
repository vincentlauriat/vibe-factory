import XCTest
@testable import VibeAPI

final class GlobalEventsTests: XCTestCase {
    func testCursorParsesAndFormatsTheCompactForm() throws {
        let cursor = try XCTUnwrap(EventCursor("1790000000123456789-3-17"))
        XCTAssertEqual(cursor, EventCursor(nanos: 1_790_000_000_123_456_789, number: 3, seq: 17))
        // Nanoseconds survive: a Date would lose the last digits.
        XCTAssertEqual(cursor.description, "1790000000123456789-3-17")
        // `since_time` of the server: the largest number and seq.
        let since = "1790000000000000000-4294967295-18446744073709551615"
        XCTAssertEqual(EventCursor(since)?.description, since)
        // Split from the right: a negative time still parses.
        XCTAssertEqual(EventCursor("-5-1-2"), EventCursor(nanos: -5, number: 1, seq: 2))
        for bad in ["", "12", "1-2", "a-1-2", "1-b-2", "1-2-c", "1-2-", "1-4294967296-0", "1-2--3"] {
            XCTAssertNil(EventCursor(bad), bad)
        }
    }

    func testCursorsOrderByTimeThenTaskThenSeq() throws {
        let cursors = ["20-1-1", "10-2-1", "10-1-9", "10-1-2"].compactMap(EventCursor.init)
        XCTAssertEqual(cursors.sorted().map(\.description), ["10-1-2", "10-1-9", "10-2-1", "20-1-1"])
        XCTAssertLessThan(EventCursor("9-9-9")!, EventCursor("10-0-0")!)
    }

    func testTaggedEnvelopesDecodeFlattened() throws {
        let events = try Fixture.decode([TaggedEnvelope].self, "events-global.json")
        XCTAssertEqual(events.count, 7)
        XCTAssertEqual(events[0].task, "9a8b7c6d-5e4f-4a3b-9c2d-1e0f9a8b7c6d")
        XCTAssertEqual(events[0].label, "#3")
        XCTAssertEqual(events[0].envelope.seq, 1)
        XCTAssertEqual(events[0].event.typeName, "run_started")
        XCTAssertEqual(events[3].event.typeName, "committed")
        guard case .unknown(let type, _) = events[5].event else { return XCTFail("expected an unknown event") }
        XCTAssertEqual(type, "hologram")
        // A line logged before 0.3: no seq, schema default kept from the payload.
        XCTAssertNil(events[6].envelope.seq)

        // Encoding writes the flat shape back.
        let data = try VibeJSON.encoder().encode(events[3])
        let object = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
        XCTAssertEqual(Set(object.keys), ["task", "number", "schema", "seq", "at", "event"])
        XCTAssertEqual(try VibeJSON.decoder().decode(TaggedEnvelope.self, from: data), events[3])
    }

    func testGroupsMatchTheWebUI() {
        XCTAssertEqual(EventGroup.allCases.map(\.rawValue),
                       ["agents", "tools", "phases", "git", "approvals", "budget", "logs"])
        // Every documented type is in exactly one group.
        let grouped = EventGroup.allCases.flatMap(\.types)
        XCTAssertEqual(Set(grouped), EventTests.documentedTypes)
        XCTAssertEqual(grouped.count, EventTests.documentedTypes.count)
        XCTAssertEqual(EventGroup.of("paused"), .approvals)
        XCTAssertEqual(EventGroup.of("committed"), .git)
        XCTAssertNil(EventGroup.of("hologram"))
    }

    /// `event-types.json` is `Event::TYPES`, checked for drift by the cargo
    /// test `event_types_fixture_is_up_to_date`. `ActivityFilter` files an
    /// unknown type under the logs (a newer server), so a type missing from
    /// `EventGroup` would pass unnoticed there: check `EventGroup.of`.
    func testEveryRustEventTypeHasAGroup() throws {
        let types = try JSONDecoder().decode([String].self, from: Fixture.data("event-types.json"))
        XCTAssertFalse(types.isEmpty)
        for type in types {
            XCTAssertNotNil(EventGroup.of(type), "`\(type)` has no EventGroup")
        }
        XCTAssertEqual(Set(EventGroup.allCases.flatMap(\.types)), Set(types), "EventGroup lists a type Rust lacks")
        XCTAssertEqual(EventTests.documentedTypes, Set(types))
    }

    func testActivityFilterKeepsGroupsAndTask() throws {
        let events = try Fixture.decode([TaggedEnvelope].self, "events-global.json")
        let types = { (filter: ActivityFilter) in events.filter(filter.keeps).map(\.event.typeName) }

        XCTAssertEqual(types(ActivityFilter()), events.map(\.event.typeName))

        var filter = ActivityFilter()
        filter.groups.remove(.budget)
        filter.groups.remove(.git)
        XCTAssertEqual(types(filter), ["run_started", "run_started", "phase_started", "hologram", "log"])

        // Unknown types go with the logs.
        filter.groups.remove(.logs)
        XCTAssertEqual(types(filter), ["run_started", "run_started", "phase_started"])

        filter = ActivityFilter(task: "1b2c3d4e-5f6a-4b7c-8d9e-0f1a2b3c4d5e")
        XCTAssertEqual(types(filter), ["run_started", "committed", "log"])
        filter.groups = [.git]
        XCTAssertEqual(types(filter), ["committed"])
    }

    func testNoticesOnePerStopOfARun() {
        let run = "r1"
        let sequence: [Event] = [
            .runStarted(run: run, task: "t"),
            .approvalRequested(run: run, gate: "plan"),
            .paused(run: run, reason: "waiting for approval"),
            .runFinished(.init(run: run, success: true, status: .planning, usage: .zero, activeMs: 0,
                               startedAt: Date(timeIntervalSince1970: 0))),
            // Resumed after the decision: same run id.
            .runStarted(run: run, task: "t"),
            .paused(run: run, reason: "budget exhausted"),
            .runFinished(.init(run: run, success: false, status: .building, usage: .zero, activeMs: 0,
                               startedAt: Date(timeIntervalSince1970: 0))),
            .runStarted(run: run, task: "t"),
            .runFinished(.init(run: run, success: true, status: .done, usage: .zero, activeMs: 0,
                               startedAt: Date(timeIntervalSince1970: 0))),
            .log(run: run, level: "error", message: "x"),
        ]
        var tracker = EventNoticeTracker()
        let notices = sequence.compactMap { tracker.notice(for: $0) }
        XCTAssertEqual(notices, [
            .approvalRequested(gate: "plan"),
            .paused(reason: "budget exhausted"),
            .finished(status: .done, success: true),
        ])
    }

    func testQueryItemsRepeatFiltersAndPreferTheLastId() {
        let query = GlobalStream.Query(after: EventCursor("1-2-3"), since: "30m", types: ["paused", "merged"],
                                       tasks: ["3", "4"])
        XCTAssertEqual(query.items().map { "\($0.name)=\($0.value ?? "")" },
                       ["after=1-2-3", "type=paused", "type=merged", "task=3", "task=4"])
        XCTAssertEqual(query.items(resumeFrom: "9-9-9").first?.value, "9-9-9")
        let sinceOnly = GlobalStream.Query(since: "2026-09-27T10:00:00+02:00")
        XCTAssertEqual(sinceOnly.items(limit: 5).map(\.name), ["since", "limit"])
        // A `+` in the query is escaped, or the server would read a space.
        let url = ServerEndpoint(baseURL: URL(string: "http://127.0.0.1:1/")!, token: "t")
            .url("events", query: sinceOnly.items())
        XCTAssertEqual(url.query, "since=2026-09-27T10:00:00%2B02:00")
    }
}

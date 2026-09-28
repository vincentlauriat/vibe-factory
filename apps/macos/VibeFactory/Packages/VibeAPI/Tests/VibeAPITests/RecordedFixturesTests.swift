import XCTest
@testable import VibeAPI

/// Answers recorded from a real `vibe serve` of this checkout, on a scratch
/// project whose task #1 ran with the scripted mock provider (the greeting
/// script of `crates/vibe-cli/tests/serve.rs`): `/api/events`, `/api/history`,
/// `/api/history/1`, `/api/tasks/1/trace` and the body of `/api/stream?since=1d`.
/// The project path is replaced by `/tmp/demo`.
final class RecordedFixturesTests: XCTestCase {
    /// `X-Vibe-Cursor` of the recorded `/api/events`.
    let recordedCursor = "1790574772257777000-1-37"

    func testRecordedEventsDecodeWithoutUnknowns() throws {
        let events = try Fixture.decode([TaggedEnvelope].self, "real-events.json")
        XCTAssertEqual(events.count, 37)
        XCTAssertTrue(events.allSatisfy { $0.number == 1 && $0.envelope.schema == 2 })
        XCTAssertEqual(events.compactMap(\.envelope.seq), Array(1...37))
        let unknown = events.filter { if case .unknown = $0.event { return true } else { return false } }
        XCTAssertEqual(unknown.map(\.event.typeName), [])
        XCTAssertEqual(events.first?.event.typeName, "run_started")
        XCTAssertEqual(events.last?.event.typeName, "run_finished")
    }

    /// The recorded stream: ids are cursors, the last one is the header of
    /// `/api/events`, and each id is the cursor of its own envelope.
    func testRecordedStreamParsesAndItsIdsAreCursors() throws {
        var parser = SSEParser()
        let messages = parser.push(Array(try Fixture.data("real-stream.txt")))
        let decoder = VibeJSON.decoder()
        let tagged = try messages.map { try decoder.decode(TaggedEnvelope.self, from: Data($0.data.utf8)) }
        XCTAssertEqual(tagged.count, 37)
        XCTAssertEqual(messages.map(\.event), tagged.map(\.event.typeName))
        let cursors = messages.compactMap { $0.id.flatMap(EventCursor.init) }
        XCTAssertEqual(cursors.count, 37)
        XCTAssertEqual(cursors.last?.description, recordedCursor)
        XCTAssertEqual(cursors, cursors.sorted())
        for (cursor, event) in zip(cursors, tagged) {
            XCTAssertEqual(cursor.number, event.number)
            XCTAssertEqual(cursor.seq, event.envelope.seq)
            // Microsecond timestamps: the cursor's time is the envelope's, to the microsecond.
            XCTAssertEqual(cursor.date.timeIntervalSince1970, event.envelope.at.timeIntervalSince1970, accuracy: 1e-5)
        }
    }

    func testRecordedHistoryAndTraceDecode() throws {
        let list = try Fixture.decode([TaskHistory].self, "real-history.json")
        XCTAssertEqual(list.map(\.number), [1])
        let h = try Fixture.decode(TaskHistory.self, "real-history-detail.json")
        XCTAssertEqual(h.task.status, .ready)
        XCTAssertEqual(h.runs.map(\.state), [.finished])
        XCTAssertTrue(h.totals.complete)
        XCTAssertEqual(h.totals.commits, 1)
        XCTAssertNil(h.cost)
        XCTAssertEqual(h.changedFiles.source, .commits)
        XCTAssertTrue(h.changedFiles.files.contains { $0.path == "hello.txt" && $0.status == .unknown })
        let row = HistoryRowText(h)
        XCTAssertEqual(row.tokens, "4.4k")
        XCTAssertEqual(row.cost, "-")
        XCTAssertTrue(row.files.hasSuffix("~"))

        let trace = try Fixture.decode(RunTrace.self, "real-trace.json")
        XCTAssertEqual(trace.run, h.runs[0].run)
        XCTAssertEqual(trace.filesWritten, ["hello.txt"])
        let write = try XCTUnwrap(trace.calls.first { $0.tool == "write_file" })
        XCTAssertEqual(write.paired, .id)
        XCTAssertTrue(write.hasOutput)
        XCTAssertEqual(write.input["path"]?.stringValue, "hello.txt")
        XCTAssertEqual(write.preview, "Created `hello.txt` (14 bytes).")
    }
}

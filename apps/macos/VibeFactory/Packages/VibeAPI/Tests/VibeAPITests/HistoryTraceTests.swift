import XCTest
@testable import VibeAPI

final class HistoryTraceTests: XCTestCase {
    func testHistoryDetailDecodes() throws {
        let h = try Fixture.decode(TaskHistory.self, "history-detail.json")
        XCTAssertEqual(h.number, 3)
        XCTAssertEqual(h.task.status, .done)
        XCTAssertEqual(h.runs.count, 2)

        let legacy = h.runs[0]
        XCTAssertEqual(legacy.state, .unknown("zombie"))
        XCTAssertFalse(legacy.totalsKnown)
        XCTAssertNil(legacy.status)
        XCTAssertEqual(legacy.pendingApproval, "plan")
        XCTAssertEqual(legacy.lastError, "process died")

        let run = h.runs[1]
        XCTAssertEqual(run.state, .finished)
        XCTAssertEqual(run.status, .done)
        XCTAssertEqual(run.resumes, 1)
        XCTAssertEqual(run.usage.total, 184_200)
        XCTAssertEqual(run.phases.map(\.phase), ["spec", "build"])
        XCTAssertNil(run.phases[1].success)
        XCTAssertEqual(run.commits[0].files, ["src/auth.rs", "src/main.rs"])
        XCTAssertEqual(run.commits[0].subtask, "st-1")
        XCTAssertEqual(run.commits[1].message, "")
        XCTAssertEqual(run.merged?.base, "main")
        XCTAssertEqual(run.validations.map(\.passed), [true, false])
        XCTAssertEqual(run.validations[0].exitCode, 0)
        XCTAssertNil(run.validations[1].exitCode)
        XCTAssertTrue(run.validations[1].integration)
        XCTAssertEqual(run.approvals.first?.comment, "ship it")

        XCTAssertEqual(h.changedFiles.source, .mergeCommit(commit: "c3d4e5f6", fastForward: true))
        XCTAssertEqual(h.changedFiles.files.map(\.status), [.added, .renamed, .unknown, .other("type_changed")])
        XCTAssertEqual(h.changedFiles.files.map(\.status.letter), ["A", "R", "?", "?"])
        XCTAssertEqual(h.changedFiles.files[1].oldPath, "src/old.rs")
        XCTAssertNil(h.changedFiles.files[0].oldPath)
        XCTAssertEqual(h.lastQa?.issues, 0)
        XCTAssertEqual(h.lastQa?.verdict, "approved")
        XCTAssertEqual(h.validationsPassed, false)
        XCTAssertEqual(h.errors.count, 1)
        // The last finished run's end.
        XCTAssertEqual(h.finishedAt, VibeJSON.parseDate("2026-09-27T09:16:12Z"))
    }

    func testHistoryListAndUnknownSources() throws {
        let list = try Fixture.decode([TaskHistory].self, "history.json")
        XCTAssertEqual(list.map(\.number), [3, 1, 4])
        XCTAssertEqual(list[1].changedFiles.source, .trace)
        XCTAssertEqual(list[2].changedFiles.source, .unknown("telepathy"))
        XCTAssertEqual(list[2].task.status, .unknown("archived"))
        // No finished run: the last activity.
        XCTAssertEqual(list[2].finishedAt, list[2].lastActivity)
    }

    func testHistoryRowsUseTheNotationOfVibeHistory() throws {
        let list = try Fixture.decode([TaskHistory].self, "history.json")
        let rows = list.map(HistoryRowText.init)

        // Totals incomplete: tokens and active are lower bounds; the cost too.
        XCTAssertEqual(rows[0].tokens, "184.2k+")
        XCTAssertEqual(rows[0].active, "6 min 12 s+")
        XCTAssertEqual(rows[0].cost, "1.83 USD+")
        XCTAssertEqual(rows[0].files, "4")
        XCTAssertEqual(rows[0].runs, "2")
        XCTAssertEqual(rows[0].commits, "2")

        // Complete totals, approximate files, no pricing.
        XCTAssertEqual(rows[1].tokens, "2.9k")
        XCTAssertEqual(rows[1].active, "4 s")
        XCTAssertEqual(rows[1].files, "1~")
        XCTAssertEqual(rows[1].cost, "-")

        XCTAssertEqual(rows[2].cost, "0.50 EUR")
    }

    func testTraceDecodes() throws {
        let trace = try Fixture.decode(RunTrace.self, "trace.json")
        XCTAssertEqual(trace.calls.count, 5)
        XCTAssertEqual(trace.filesWritten, ["hello.txt"])
        XCTAssertEqual(trace.errorCount, 1)
        XCTAssertEqual(trace.calls.map(\.paired), [.id, .id, .order, .unmatched, .unknown("psychic")])

        let write = trace.calls[0]
        XCTAssertEqual(write.call, "5f0c2a9e1b7d")
        XCTAssertEqual(write.input["path"]?.stringValue, "hello.txt")
        XCTAssertEqual(write.durationMs, 2)
        XCTAssertTrue(write.hasOutput)

        let command = trace.calls[1]
        XCTAssertEqual(command.role, "qa_fixer")
        XCTAssertNil(command.subtask)
        XCTAssertEqual(command.exitCode, 101)
        XCTAssertTrue(command.timedOut)
        XCTAssertEqual(command.isError, true)

        // Logged before 0.5: nil id, nothing to load.
        XCTAssertEqual(trace.calls[2].call, TraceCall.nilCall)
        XCTAssertFalse(trace.calls[2].hasOutput)
        // Never returned.
        XCTAssertNil(trace.calls[3].returnedAt)
        XCTAssertNil(trace.calls[3].isError)

        let all = try Fixture.decode([RunTrace].self, "trace-all.json")
        XCTAssertEqual(all.map(\.calls.count), [0, 5])
    }
}

import XCTest
@testable import VibeAPI

final class ModelsTests: XCTestCase {
    func testDatesInEveryFormChronoWrites() throws {
        let whole = try XCTUnwrap(VibeJSON.parseDate("2026-09-27T10:00:00Z"))
        XCTAssertEqual(whole.timeIntervalSince1970, 1_790_503_200, accuracy: 0.001)
        let micro = try XCTUnwrap(VibeJSON.parseDate("2026-09-27T10:00:00.545340Z"))
        XCTAssertEqual(micro.timeIntervalSince(whole), 0.54534, accuracy: 0.000_001)
        let nano = try XCTUnwrap(VibeJSON.parseDate("2026-09-27T10:00:00.000000001Z"))
        XCTAssertEqual(nano.timeIntervalSince(whole), 0, accuracy: 0.000_001)
        let offset = try XCTUnwrap(VibeJSON.parseDate("2026-09-27T12:00:00.5+02:00"))
        XCTAssertEqual(offset.timeIntervalSince(whole), 0.5, accuracy: 0.000_001)
        XCTAssertEqual(VibeJSON.parseDate("1970-01-01T00:00:00Z")?.timeIntervalSince1970, 0)
        XCTAssertNil(VibeJSON.parseDate("yesterday"))
    }

    func testTaskRows() throws {
        let rows = try Fixture.decode([TaskRow].self, "tasks.json")
        XCTAssertEqual(rows.count, 4)

        XCTAssertEqual(rows[0].label, "#3")
        XCTAssertEqual(rows[0].task.status, .review)
        XCTAssertEqual(rows[0].task.branch, "vibe/003-add-login")
        XCTAssertEqual(rows[0].task.complexity, "standard")
        XCTAssertTrue(rows[0].running)
        XCTAssertEqual(rows[0].run?.status, .running)
        XCTAssertEqual(rows[0].run?.currentPhase, "qa")
        XCTAssertNil(rows[0].run?.pendingApproval)

        XCTAssertNil(rows[1].number)
        XCTAssertEqual(rows[1].label, "11111111")
        XCTAssertNil(rows[1].run)
        XCTAssertNil(rows[1].task.branch)
        XCTAssertEqual(rows[1].task.source?["provider"], .string("github"))

        XCTAssertEqual(rows[2].run?.pendingApproval, "spec")
        XCTAssertEqual(rows[2].task.labels, [])

        XCTAssertEqual(rows[3].task.status, .unknown("archived"))
        XCTAssertFalse(rows[3].task.status.isTerminal)
    }

    func testTaskDetailWithEverything() throws {
        let detail = try Fixture.decode(TaskDetail.self, "task-detail.json")
        let run = try XCTUnwrap(detail.run)
        XCTAssertEqual(run.status, .paused)
        XCTAssertEqual(run.pendingApproval, "merge")
        XCTAssertEqual(run.completedPhases.map(\.rawValue), ["assess", "spec", "plan", "build", "qa"])
        XCTAssertEqual(run.usage.inputTokens, 114_472)
        XCTAssertEqual(run.usage.total, 114_472 + 11_729)
        XCTAssertEqual(run.activeMs, 189_596)
        XCTAssertEqual(run.validations.first?.passed, true)
        XCTAssertEqual(run.approvals.first?.gate, "spec")
        XCTAssertEqual(detail.row.run?.pendingApproval, "merge")

        let spec = try XCTUnwrap(detail.spec)
        XCTAssertEqual(spec.requirements.count, 2)
        XCTAssertEqual(spec.requirements[1].priority, 1)
        XCTAssertEqual(spec.requirements[1].kind, "functional")
        XCTAssertEqual(spec.requirements[0].acceptance, ["button visible", "redirects"])

        let plan = try XCTUnwrap(detail.plan)
        XCTAssertEqual(plan.phases.count, 2)
        XCTAssertEqual(plan.subtasks.map(\.status.rawValue), ["done", "pending"])
        XCTAssertEqual(detail.subtaskTitle("2b3c4d5e-6f70-4812-a3b4-c5d6e7f80912"), "Route")
        XCTAssertEqual(detail.subtaskTitle("ffffffff-0000"), "ffffffff")

        let qa = try XCTUnwrap(detail.qaReports.first)
        XCTAssertEqual(qa.verdict.display, "changes requested")
        XCTAssertEqual(qa.issues.first?.line, 42)
        XCTAssertEqual(qa.issues.first?.suggestedFix, "Redact it")
    }

    func testTaskDetailOfANewTask() throws {
        let detail = try Fixture.decode(TaskDetail.self, "task-detail-new.json")
        XCTAssertNil(detail.run)
        XCTAssertNil(detail.spec)
        XCTAssertNil(detail.plan)
        XCTAssertTrue(detail.qaReports.isEmpty)
        XCTAssertNil(detail.row.run)
    }

    func testEvals() throws {
        let evals = try Fixture.decode(EvalsResponse.self, "evals.json")
        XCTAssertTrue(evals.enabled)
        let suite = try XCTUnwrap(evals.suites.first)
        XCTAssertNotNil(suite.modified)
        let rows = suite.rows
        XCTAssertEqual(rows.count, 2)
        XCTAssertEqual(rows[1].case, "ALL")
        XCTAssertEqual(rows[1].successRate, 0.75)
        XCTAssertNil(rows[0].model)
        XCTAssertEqual(suite.summary["reports"], .number(4))

        let disabled = try VibeJSON.decoder().decode(
            EvalsResponse.self, from: Data(#"{"enabled":false,"suites":[]}"#.utf8))
        XCTAssertFalse(disabled.enabled)
    }

    func testOpenEnumsRoundTrip() throws {
        let data = try JSONEncoder().encode([TaskStatus.done, .unknown("later")])
        XCTAssertEqual(String(decoding: data, as: UTF8.self), #"["done","later"]"#)
        XCTAssertEqual(try JSONDecoder().decode([TaskStatus].self, from: data), [.done, .unknown("later")])
        let name = try JSONEncoder().encode(Name(rawValue: "in_progress"))
        XCTAssertEqual(String(decoding: name, as: UTF8.self), #""in_progress""#)
    }
}

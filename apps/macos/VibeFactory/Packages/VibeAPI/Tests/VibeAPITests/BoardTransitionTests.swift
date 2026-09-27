import XCTest
@testable import VibeAPI

final class BoardTransitionTests: XCTestCase {
    private func row(_ id: String, running: Bool = false, status: RunStatus? = nil, gate: Name? = nil,
                     taskStatus: String = "building") throws -> TaskRow {
        let task = try VibeJSON.decoder().decode(VibeTask.self, from: Data("""
            {"id":"\(id)","title":"T","description":"","status":"\(taskStatus)","complexity":null,
             "created_at":"2026-09-27T10:00:00Z","updated_at":"2026-09-27T10:00:00Z"}
            """.utf8))
        return TaskRow(task: task, number: 1, running: running,
                       run: status.map { RunSummary(status: $0, currentPhase: "build", pendingApproval: gate) })
    }

    func testTransitions() throws {
        let before = [
            try row("a", running: true, status: .running),
            try row("b", running: true, status: .running),
            try row("c", running: true, status: .running),
            try row("d", running: true, status: .running),
        ]
        let after = [
            try row("a", running: false, status: .paused, gate: "plan"),
            try row("b", running: false, status: .paused),
            try row("c", running: false, status: .finished, taskStatus: "ready"),
            try row("d", running: true, status: .running),
            try row("e", running: true, status: .running), // new: nothing
        ]
        let transitions = BoardTransition.between(before, after)
        XCTAssertEqual(transitions.count, 3)
        guard case .approvalRequested(let a, let gate) = transitions[0] else { return XCTFail() }
        XCTAssertEqual(a.id, "a")
        XCTAssertEqual(gate, "plan")
        guard case .paused(let b) = transitions[1] else { return XCTFail() }
        XCTAssertEqual(b.id, "b")
        guard case .finished(let c) = transitions[2] else { return XCTFail() }
        XCTAssertEqual(c.task.status, .ready)
    }

    func testFirstLoadAndNoChangeProduceNothing() throws {
        let rows = [try row("a", running: true, status: .running)]
        XCTAssertTrue(BoardTransition.between([], rows).isEmpty)
        XCTAssertTrue(BoardTransition.between(rows, rows).isEmpty)
        let waiting = [try row("a", status: .paused, gate: "spec")]
        XCTAssertTrue(BoardTransition.between(waiting, waiting).isEmpty)
    }
}

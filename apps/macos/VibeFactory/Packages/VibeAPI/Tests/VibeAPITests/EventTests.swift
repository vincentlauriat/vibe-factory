import XCTest
@testable import VibeAPI

final class EventTests: XCTestCase {
    /// Every type of `events.md`, in the 0.5 shape.
    static let documentedTypes: Set<String> = [
        "run_started", "phase_started", "phase_finished", "agent_started", "agent_delta", "agent_text",
        "tool_called", "tool_returned", "agent_finished", "subtask_updated", "subtask_integrated",
        "committed", "merged", "validation_finished", "budget_updated", "artefact_written",
        "approval_requested", "approval_resolved", "retrying", "paused", "run_finished", "log",
    ]

    func testEveryDocumentedTypeDecodes() throws {
        let envelopes = try Fixture.envelopes("events-0.5.jsonl")
        let known = envelopes.filter {
            if case .unknown = $0.event { return false }
            return true
        }
        XCTAssertEqual(Set(known.map(\.event.typeName)), Self.documentedTypes)
        let unknown = envelopes.compactMap { envelope -> String? in
            if case .unknown(let type, _) = envelope.event { return type }
            return nil
        }
        XCTAssertEqual(unknown, ["future_thing"])
    }

    func testFieldsOfThe05Additions() throws {
        let envelopes = try Fixture.envelopes("events-0.5.jsonl")
        let events = envelopes.map(\.event)

        guard case .agentStarted(_, let role, let subtask, let model) = events[2] else { return XCTFail() }
        XCTAssertEqual(role, "coder")
        XCTAssertNotNil(subtask)
        XCTAssertEqual(model, "claude-sonnet-5")

        XCTAssertNil(envelopes[3].seq)
        XCTAssertTrue(events[3].isEphemeral)
        guard case .agentDelta(_, _, _, let delta) = events[3] else { return XCTFail() }
        XCTAssertEqual(delta, StreamDelta(kind: "text", text: "Reading the "))

        guard case .toolCalled(let call) = events[5] else { return XCTFail() }
        XCTAssertEqual(call.call, "a1b2c3d4e5f6")
        XCTAssertEqual(call.input["command"], .string("cargo test"))

        guard case .toolReturned(let ret) = events[6] else { return XCTFail() }
        XCTAssertEqual(ret.call, call.call)
        XCTAssertEqual(ret.exitCode, 101)
        XCTAssertFalse(ret.timedOut)
        XCTAssertEqual(ret.outputChars, 48_213)
        XCTAssertEqual(ret.outputFile?.hasPrefix(".vibe/tool-output/"), true)

        guard case .committed(_, let committedSubtask, _, let message, let files) = events[10] else { return XCTFail() }
        XCTAssertNil(committedSubtask)
        XCTAssertEqual(message, "fix: handle empty login")
        XCTAssertEqual(files.count, 2)

        guard case .budgetUpdated(_, let tokens, let limit, _, let durationLimit) = events[12] else { return XCTFail() }
        XCTAssertEqual(tokens, 1250)
        XCTAssertEqual(limit, 200_000)
        XCTAssertNil(durationLimit)

        guard case .artefactWritten(_, .qaReport(let round)) = events[13] else { return XCTFail() }
        XCTAssertEqual(round, 2)

        guard case .merged(_, _, let branch, let base) = events[18] else { return XCTFail() }
        XCTAssertEqual(branch, "vibe/003-add-login")
        XCTAssertEqual(base, "main")

        guard case .runFinished(let totals) = events[21] else { return XCTFail() }
        XCTAssertEqual(totals.status, .done)
        XCTAssertEqual(totals.usage.outputTokens, 250)
        XCTAssertEqual(totals.activeMs, 23_000)
        XCTAssertEqual(totals.startedAt, VibeJSON.parseDate("2026-09-27T10:00:00.123456Z"))

        guard case .log(let run, _, _) = events[22] else { return XCTFail() }
        XCTAssertNil(run)
        XCTAssertNil(events[22].runId)

        guard case .unknown(_, let payload) = events[23] else { return XCTFail() }
        XCTAssertEqual(payload["answer"], .number(42))
        XCTAssertEqual(envelopes[23].schema, 3)
        XCTAssertEqual(events[23].runId, "5f0c1d2e-3a4b-4c5d-8e9f-0a1b2c3d4e5f")
    }

    func testLogsWrittenBefore05UseTheDocumentedDefaults() throws {
        let envelopes = try Fixture.envelopes("events-legacy.jsonl")
        XCTAssertFalse(envelopes.contains {
            if case .unknown = $0.event { return true }
            return false
        })

        guard case .agentStarted(_, _, let subtask, let model) = envelopes[2].event else { return XCTFail() }
        XCTAssertNil(subtask)
        XCTAssertEqual(model, "")

        guard case .toolCalled(let call) = envelopes[6].event else { return XCTFail() }
        XCTAssertEqual(call.call, "000000000000")
        XCTAssertNil(call.subtask)

        guard case .toolReturned(let ret) = envelopes[7].event else { return XCTFail() }
        XCTAssertNil(ret.exitCode)
        XCTAssertNil(ret.outputFile)
        XCTAssertFalse(ret.timedOut)
        XCTAssertEqual(ret.outputChars, 0)

        guard case .runFinished(let totals) = envelopes[10].event else { return XCTFail() }
        XCTAssertFalse(totals.success)
        XCTAssertEqual(totals.status, .review)
        XCTAssertEqual(totals.usage, .zero)
        XCTAssertEqual(totals.activeMs, 0)
        XCTAssertEqual(totals.startedAt.timeIntervalSince1970, 0)

        // Logs written before 0.3 have no `schema`.
        XCTAssertEqual(envelopes[11].schema, 1)
    }

    func testAKnownTypeWithBrokenFieldsBecomesUnknown() throws {
        let json = #"{"schema":2,"seq":1,"at":"2026-09-27T10:00:00Z","event":{"type":"phase_started","phase":"build"}}"#
        let envelope = try VibeJSON.decoder().decode(Envelope.self, from: Data(json.utf8))
        guard case .unknown(let type, let payload) = envelope.event else { return XCTFail() }
        XCTAssertEqual(type, "phase_started")
        XCTAssertEqual(payload["phase"], .string("build"))
    }

    func testEncodingRoundTrips() throws {
        let envelopes = try Fixture.envelopes("events-0.5.jsonl") + Fixture.envelopes("events-legacy.jsonl")
        let encoder = VibeJSON.encoder()
        let decoder = VibeJSON.decoder()
        for envelope in envelopes {
            let again = try decoder.decode(Envelope.self, from: encoder.encode(envelope))
            XCTAssertEqual(again.event, envelope.event, envelope.event.typeName)
            XCTAssertEqual(again.seq, envelope.seq)
        }
    }
}

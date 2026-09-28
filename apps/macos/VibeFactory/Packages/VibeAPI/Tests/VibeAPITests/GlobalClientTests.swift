import XCTest
@testable import VibeAPI

/// The project-wide routes of the client: `/api/events`, `/api/stream`,
/// `/api/history`, the trace and call outputs.
final class GlobalClientTests: XCTestCase {
    private let endpoint = ServerEndpoint(baseURL: URL(string: "http://127.0.0.1:7777/")!, token: "secret")

    private func client() -> VibeClient {
        VibeClient(endpoint: endpoint, session: StubProtocol.session())
    }

    private func reply(_ data: Data, status: Int = 200, headers: [String: String] = [:]) -> StubProtocol.Reply {
        StubProtocol.Reply(status: status, headers: ["Content-Type": "application/json"].merging(headers) { $1 },
                           chunks: [data])
    }

    private func json(_ text: String, status: Int = 200) -> StubProtocol.Reply {
        reply(Data(text.utf8), status: status)
    }

    func testEventsReturnsTheHeaders() async throws {
        let events = try Fixture.data("events-global.json")
        StubProtocol.install { request in
            if request.query("type") == "nope" { return self.json(#"{"error":"unknown event type `nope`"}"#, status: 400) }
            if request.query("limit") == "0" { return self.json("[]") }
            return self.reply(events, headers: ["X-Vibe-Cursor": "1790000000123456789-3-4", "X-Vibe-Read-Errors": "2"])
        }
        let client = client()

        let page = try await client.events(after: EventCursor("1-2-3"), types: ["committed", "paused"],
                                           tasks: ["3"], limit: 50)
        XCTAssertEqual(page.events.count, 7)
        XCTAssertEqual(page.cursor?.description, "1790000000123456789-3-4")
        XCTAssertEqual(page.readErrors, 2)
        let request = StubProtocol.requests[0]
        XCTAssertEqual(request.url?.path, "/api/events")
        XCTAssertEqual(request.url?.query, "after=1-2-3&type=committed&type=paused&task=3&limit=50")
        XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer secret")

        // Nothing read: no cursor header.
        let empty = try await client.events(limit: 0)
        XCTAssertNil(empty.cursor)
        XCTAssertEqual(empty.readErrors, 0)

        do {
            _ = try await client.events(types: ["nope"])
            XCTFail("expected an error")
        } catch let error as VibeError {
            XCTAssertEqual(error, .server(status: 400, message: "unknown event type `nope`"))
        }
    }

    func testHistoryAndTraceRoutes() async throws {
        let history = try Fixture.data("history.json")
        let detail = try Fixture.data("history-detail.json")
        let trace = try Fixture.data("trace.json")
        let traces = try Fixture.data("trace-all.json")
        StubProtocol.install { request in
            switch (request.url?.path ?? "", request.query("all"), request.query("run")) {
            case ("/api/history", _, _): return self.reply(history)
            case ("/api/history/3", _, _): return self.reply(detail)
            case ("/api/tasks/3/trace", "true", nil): return self.reply(traces)
            case ("/api/tasks/3/trace", "true", _):
                return self.json(#"{"error":"`run` and `all` exclude each other"}"#, status: 400)
            case ("/api/tasks/3/trace", nil, "5f"): return self.json(#"{"error":"`5f` matches several runs of this task"}"#, status: 400)
            case ("/api/tasks/3/trace", nil, _): return self.reply(trace)
            case ("/api/tasks/9/trace", _, _): return self.json(#"{"error":"the task has not been run yet"}"#, status: 404)
            default: return self.json(#"{"error":"no route"}"#, status: 404)
            }
        }
        let client = client()

        let all = try await client.history(all: true)
        let finished = try await client.history()
        let one = try await client.history(task: "3")
        let last = try await client.trace(task: "3")
        let byRun = try await client.trace(task: "3", run: "5f0c1d2e")
        let every = try await client.trace(task: "3", all: true)

        XCTAssertEqual(all.count, 3)
        XCTAssertEqual(finished.count, 3)
        XCTAssertEqual(one.number, 3)
        XCTAssertEqual(last.map(\.calls.count), [5])
        XCTAssertEqual(byRun.count, 1)
        XCTAssertEqual(every.map(\.calls.count), [0, 5])
        let requests = StubProtocol.requests
        XCTAssertEqual(requests[0].query("all"), "true")
        XCTAssertNil(requests[1].query("all"))
        XCTAssertEqual(requests[4].query("run"), "5f0c1d2e")

        do {
            _ = try await client.trace(task: "3", run: "5f")
            XCTFail("expected an error")
        } catch let error as VibeError {
            XCTAssertEqual(error, .server(status: 400, message: "`5f` matches several runs of this task"))
        }
        do {
            _ = try await client.trace(task: "3", run: "x", all: true)
            XCTFail("expected an error")
        } catch let error as VibeError {
            XCTAssertEqual(error, .server(status: 400, message: "`run` and `all` exclude each other"))
        }
        do {
            _ = try await client.trace(task: "9")
            XCTFail("expected an error")
        } catch let error as VibeError {
            XCTAssertEqual(error, .notFound("the task has not been run yet"))
        }
    }

    func testTraceOutputIsTextAndMissingIsNil() async throws {
        StubProtocol.install { request in
            switch request.url?.path ?? "" {
            case "/api/tasks/3/trace/5f0c2a9e1b7d/output":
                return StubProtocol.Reply(status: 200, headers: ["Content-Type": "text/plain; charset=utf-8"],
                                          chunks: [Data("line 1\nlïne 2\n".utf8)])
            case "/api/tasks/3/trace/6a1d3b0f2c8e/output":
                return StubProtocol.Reply(status: 200, headers: ["Content-Type": "text/plain; charset=utf-8",
                                                                 "X-Vibe-Truncated": "true"],
                                          chunks: [Data("cut\n… (output cut: 9 more bytes)\n".utf8)])
            case "/api/tasks/3/trace/000000000000/output":
                return self.json(#"{"error":"invalid call id `000000000000` (12 hex digits expected)"}"#, status: 400)
            default:
                return self.json(#"{"error":"the output of this call was not traced, or was discarded"}"#, status: 404)
            }
        }
        let client = client()

        let output = try await client.traceOutput(task: "3", call: "5f0c2a9e1b7d")
        XCTAssertEqual(output, CallOutput(text: "line 1\nlïne 2\n", truncated: false))
        let cut = try await client.traceOutput(task: "3", call: "6a1d3b0f2c8e")
        XCTAssertEqual(cut?.truncated, true)
        let missing = try await client.traceOutput(task: "3", call: "7b2e4c1a3d9f")
        XCTAssertNil(missing)
        do {
            _ = try await client.traceOutput(task: "3", call: "000000000000")
            XCTFail("expected an error")
        } catch let error as VibeError {
            XCTAssertEqual(error, .server(status: 400, message: "invalid call id `000000000000` (12 hex digits expected)"))
        }
        XCTAssertEqual(StubProtocol.requests[0].value(forHTTPHeaderField: "Accept"), "text/plain")
    }

    /// The replay after the starting cursor, streamed text without id, a cut,
    /// then the live part resumed from the last logged id.
    func testGlobalStreamReplaysThenResumesFromTheLastId() async throws {
        func frame(_ id: String?, _ type: String, task: Int, seq: Int?, _ fields: String) -> String {
            let seqField = seq.map { #""seq":\#($0),"# } ?? ""
            return "event: \(type)\n" + (id.map { "id: \($0)\n" } ?? "")
                + "data: {\"task\":\"task-\(task)\",\"number\":\(task),\"schema\":2,\(seqField)"
                + "\"at\":\"2026-09-27T10:00:00.123456Z\",\"event\":{\"type\":\"\(type)\",\"run\":\"r\(task)\"\(fields)}}\n\n"
        }
        let replay = frame("1790000000000000001-3-5", "phase_started", task: 3, seq: 5, #","phase":"build""#)
            + ": keep-alive\n\n"
            + frame("1790000000000000001-4-1", "run_started", task: 4, seq: 1, #","task":"task-4""#)
            + frame(nil, "agent_delta", task: 3, seq: nil,
                    #","role":"coder","subtask":null,"delta":{"kind":"text","text":"hi"}"#)
        let live = frame("1790000000000000002-3-6", "paused", task: 3, seq: 6, #","reason":"waiting""#)
        let counter = Counter()
        StubProtocol.install { _ in
            let n = counter.next()
            let body = n == 0 ? replay : n == 1 ? live : ""
            let bytes = Array(body.utf8)
            let chunks = stride(from: 0, to: bytes.count, by: 11).map { Data(bytes[$0..<min($0 + 11, bytes.count)]) }
            return StubProtocol.Reply(status: 200, headers: ["Content-Type": "text/event-stream"], chunks: chunks)
        }
        let stream = client().globalStream(after: EventCursor("1790000000000000000-9-9"), types: ["paused"],
                                           backoff: Backoff(initial: 0.01, maximum: 0.01))
        var received: [GlobalEvent] = []
        for try await event in stream.events() {
            received.append(event)
            if received.count == 4 { break }
        }
        XCTAssertEqual(received.map(\.tagged.event.typeName), ["phase_started", "run_started", "agent_delta", "paused"])
        XCTAssertEqual(received.map(\.tagged.number), [3, 4, 3, 3])
        XCTAssertEqual(received.map { $0.cursor?.description },
                       ["1790000000000000001-3-5", "1790000000000000001-4-1", nil, "1790000000000000002-3-6"])

        let requests = StubProtocol.requests
        XCTAssertEqual(requests[0].url?.path, "/api/stream")
        XCTAssertEqual(requests[0].query("after"), "1790000000000000000-9-9")
        XCTAssertNil(requests[0].value(forHTTPHeaderField: "Last-Event-ID"))
        XCTAssertEqual(requests[0].value(forHTTPHeaderField: "Authorization"), "Bearer secret")
        XCTAssertNil(requests[0].query("token"))
        // The delta did not move the resume point.
        XCTAssertEqual(requests[1].value(forHTTPHeaderField: "Last-Event-ID"), "1790000000000000001-4-1")
        XCTAssertEqual(requests[1].query("after"), "1790000000000000001-4-1")
        XCTAssertEqual(requests[1].query("type"), "paused")
    }

    /// A 503 (too many streams) waits with the saturated delays, a 500 with
    /// the ordinary ones; the count restarts when the kind of failure changes,
    /// so the first 503 after ordinary failures waits 5 s, not the maximum.
    func testGlobalStreamWaitsLongerWhenTheServerIsSaturated() async throws {
        let counter = Counter()
        StubProtocol.install { _ in
            switch counter.next() {
            case 0, 1, 4: return self.json(#"{"error":"boom"}"#, status: 500)
            case 2, 3: return self.json(#"{"error":"too many event streams are open"}"#, status: 503)
            default:
                let frame = "event: paused\nid: 1-3-6\ndata: {\"task\":\"t\",\"number\":3,\"schema\":2,\"seq\":6,"
                    + "\"at\":\"2026-09-27T10:00:00Z\",\"event\":{\"type\":\"paused\",\"run\":\"r\",\"reason\":\"x\"}}\n\n"
                return StubProtocol.Reply(status: 200, headers: ["Content-Type": "text/event-stream"],
                                          chunks: [Data(frame.utf8)])
            }
        }
        let delays = Recorder()
        let backoff = Backoff(initial: 0.5, maximum: 10, saturatedInitial: 5, saturatedMaximum: 60) { delays.add($0) }
        for try await event in client().globalStream(backoff: backoff).events() {
            XCTAssertEqual(event.cursor?.description, "1-3-6")
            break
        }
        // The sleep does not wait: the closed body may have reconnected
        // already, from attempt 0 again since a message came through.
        XCTAssertEqual(Array(delays.values.prefix(5)), [0.5, 1, 5, 10, 0.5])
        XCTAssertEqual(delays.values.dropFirst(5).first ?? 0.5, 0.5)
    }

    func testGlobalStreamStopsOnABadRequest() async throws {
        StubProtocol.install { _ in self.json(#"{"error":"invalid event cursor `x`"}"#, status: 400) }
        let stream = client().globalStream(backoff: Backoff(initial: 0.01, maximum: 0.01))
        do {
            for try await _ in stream.events() {}
            XCTFail("expected an error")
        } catch let error as VibeError {
            XCTAssertEqual(error, .server(status: 400, message: "invalid event cursor `x`"))
        }
    }
}

/// Delays asked of an injected `Backoff.sleep`, which does not wait.
final class Recorder: @unchecked Sendable {
    private let lock = NSLock()
    private var recorded: [TimeInterval] = []

    func add(_ value: TimeInterval) { lock.withLock { recorded.append(value) } }
    var values: [TimeInterval] { lock.withLock { recorded } }
}

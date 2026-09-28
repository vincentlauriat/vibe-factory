import XCTest
@testable import VibeAPI

final class VibeClientTests: XCTestCase {
    private let endpoint = ServerEndpoint(baseURL: URL(string: "http://127.0.0.1:7777/")!, token: "secret")

    private func client() -> VibeClient {
        VibeClient(endpoint: endpoint, session: StubProtocol.session())
    }

    private func json(_ text: String, status: Int = 200) -> StubProtocol.Reply {
        StubProtocol.Reply(status: status, chunks: [Data(text.utf8)])
    }

    func testRoutesMethodsAndToken() async throws {
        let tasksData = try Fixture.data("tasks.json")
        let detailData = try Fixture.data("task-detail.json")
        let row = String(decoding: try JSONSerialization.data(withJSONObject:
            (try JSONSerialization.jsonObject(with: tasksData) as! [Any])[0]), as: UTF8.self)
        StubProtocol.install { request in
            switch (request.httpMethod ?? "", request.url?.path ?? "") {
            case ("GET", "/api/health"): return self.json(#"{"name":"vibe","version":"0.5.0"}"#)
            case ("GET", "/api/tasks"): return StubProtocol.Reply(status: 200, chunks: [tasksData])
            case ("POST", "/api/tasks"): return self.json(row, status: 201)
            case ("GET", "/api/tasks/3"): return StubProtocol.Reply(status: 200, chunks: [detailData])
            case ("POST", "/api/tasks/3/run"): return self.json(#"{"task_id":"t","resumed":true}"#, status: 202)
            case ("POST", "/api/tasks/3/cancel"): return self.json(#"{"task_id":"t"}"#, status: 202)
            case ("POST", "/api/tasks/3/approve"), ("POST", "/api/tasks/3/reject"):
                let approved = request.url!.path.hasSuffix("approve")
                return self.json(#"{"gate":"merge","approved":\#(approved),"comment":"c","at":"2026-09-27T10:00:00Z"}"#)
            case ("GET", "/api/tasks/3/changes"): return self.json(#"{"changes":" src/a.rs | 2 +-\n"}"#)
            case ("GET", "/api/tasks/3/events"): return self.json("[]")
            case ("GET", "/api/evals"): return self.json(#"{"enabled":false,"suites":[]}"#)
            default: return self.json(#"{"error":"no route"}"#, status: 404)
            }
        }
        let client = client()

        let health = try await client.health()
        let tasks = try await client.tasks()
        let created = try await client.createTask(title: "Add login", description: "d")
        let detail = try await client.task("3")
        let accepted = try await client.run("3", resume: true)
        try await client.cancel("3")
        let approval = try await client.approve("3", comment: "ok")
        let rejection = try await client.reject("3", reason: "no")
        let changes = try await client.changes("3")
        let events = try await client.events("3", after: 12, all: true)
        let evals = try await client.evals()

        XCTAssertEqual(health.version, "0.5.0")
        XCTAssertEqual(tasks.count, 4)
        XCTAssertEqual(created.label, "#3")
        XCTAssertEqual(detail.plan?.phases.count, 2)
        XCTAssertTrue(accepted.resumed)
        XCTAssertTrue(approval.approved)
        XCTAssertFalse(rejection.approved)
        XCTAssertEqual(changes, " src/a.rs | 2 +-\n")
        XCTAssertEqual(events.count, 0)
        XCTAssertFalse(evals.enabled)

        let requests = StubProtocol.requests
        XCTAssertEqual(requests.count, 11)
        XCTAssertTrue(requests.allSatisfy { $0.value(forHTTPHeaderField: "Authorization") == "Bearer secret" })
        XCTAssertEqual(requests[2].bodyJSON?["title"] as? String, "Add login")
        XCTAssertEqual(requests[2].value(forHTTPHeaderField: "Content-Type"), "application/json")
        XCTAssertEqual(requests[4].bodyJSON?["resume"] as? Bool, true)
        XCTAssertEqual(requests[6].bodyJSON?["comment"] as? String, "ok")
        XCTAssertEqual(requests[7].bodyJSON?["reason"] as? String, "no")
        XCTAssertEqual(requests[9].query("after"), "12")
        XCTAssertEqual(requests[9].query("all"), "true")
    }

    func testErrorsAreTyped() async throws {
        StubProtocol.install { request in
            switch request.url?.path ?? "" {
            case "/api/tasks": return self.json(#"{"error":"missing or wrong token"}"#, status: 401)
            case "/api/tasks/zzz": return self.json(#"{"error":"no task matches `zzz`"}"#, status: 404)
            case "/api/tasks/3/run": return self.json(#"{"error":"a run is already active"}"#, status: 409)
            default: return StubProtocol.Reply(status: 200, chunks: [Data("not json".utf8)])
            }
        }
        let client = client()
        await assertThrows(VibeError.unauthorized) { _ = try await client.tasks() }
        await assertThrows(VibeError.notFound("no task matches `zzz`")) { _ = try await client.task("zzz") }
        await assertThrows(VibeError.server(status: 409, message: "a run is already active")) {
            try await client.run("3")
        }
        do {
            _ = try await client.health()
            XCTFail("expected an error")
        } catch VibeError.transport {
        }
    }

    func testCancellingARequestIsACancellationNotATransportError() async throws {
        StubProtocol.install { _ in StubProtocol.Reply(status: 200, chunks: [], hang: true) }
        let client = client()
        let request = Task { try await client.task("3") }
        try await Task.sleep(nanoseconds: 100_000_000)
        request.cancel()
        do {
            _ = try await request.value
            XCTFail("expected a cancellation")
        } catch is CancellationError {
        } catch {
            XCTFail("unexpected \(error)")
        }
    }

    func testStreamReconnectsFromTheLastSeqAndDropsDuplicates() async throws {
        let run = "5f0c1d2e-3a4b-4c5d-8e9f-0a1b2c3d4e5f"
        func event(_ seq: Int?, _ type: String, _ fields: String = "") -> String {
            let id = seq.map { "id: \($0)\n" } ?? ""
            let seqField = seq.map { #""seq":\#($0),"# } ?? ""
            return "event: \(type)\n\(id)data: {\"schema\":2,\(seqField)\"at\":\"2026-09-27T10:00:00Z\",\"event\":{\"type\":\"\(type)\",\"run\":\"\(run)\"\(fields)}}\n\n"
        }
        let first = event(1, "phase_started", #","phase":"build""#)
            + ": keep-alive\n\n"
            + event(nil, "agent_delta", #","role":"coder","subtask":null,"delta":{"kind":"text","text":"hi"}"#)
        let second = event(1, "phase_started", #","phase":"build""#) // already seen
            + event(2, "paused", #","reason":"waiting""#)
        let counter = Counter()
        StubProtocol.install { request in
            let n = counter.next()
            let body = n == 0 ? first : n == 1 ? second : ""
            // Split the body in awkward chunks.
            let bytes = Array(body.utf8)
            let chunks = stride(from: 0, to: bytes.count, by: 7).map { Data(bytes[$0..<min($0 + 7, bytes.count)]) }
            return StubProtocol.Reply(status: 200, headers: ["Content-Type": "text/event-stream"], chunks: chunks)
        }
        let stream = EventStream(endpoint: endpoint, path: "tasks/3/stream", session: StubProtocol.session(),
                                 backoff: Backoff(initial: 0.01, maximum: 0.01)) { run }
        var received: [String] = []
        for try await envelope in stream.envelopes() {
            received.append(envelope.event.typeName)
            if received.count == 3 { break }
        }
        XCTAssertEqual(received, ["phase_started", "agent_delta", "paused"])
        let requests = StubProtocol.requests
        XCTAssertNil(requests[0].query("after"))
        XCTAssertEqual(requests[1].query("after"), "1")
        XCTAssertEqual(requests[1].value(forHTTPHeaderField: "Authorization"), "Bearer secret")
        XCTAssertEqual(requests[1].value(forHTTPHeaderField: "Last-Event-ID"), "1")
    }

    func testStreamRestartsWhenANewRunBegan() async throws {
        let counter = Counter()
        StubProtocol.install { _ in
            let n = counter.next()
            let run = n == 0 ? "run-a" : "run-b"
            let body = "id: 4\ndata: {\"seq\":4,\"at\":\"2026-09-27T10:00:00Z\",\"event\":{\"type\":\"phase_started\",\"run\":\"\(run)\",\"phase\":\"qa\"}}\n\n"
            return StubProtocol.Reply(status: 200, chunks: n < 2 ? [Data(body.utf8)] : [])
        }
        let stream = EventStream(endpoint: endpoint, path: "tasks/3/stream", session: StubProtocol.session(),
                                 backoff: Backoff(initial: 0.01, maximum: 0.01)) { "run-b" }
        var runs: [String?] = []
        for try await envelope in stream.envelopes() {
            runs.append(envelope.event.runId)
            if runs.count == 2 { break }
        }
        // Same seq, new run: delivered, and the reconnection asked from the start.
        XCTAssertEqual(runs, ["run-a", "run-b"])
        XCTAssertNil(StubProtocol.requests[1].query("after"))
    }

    func testStreamReplaysFromTheStartWhenTheCurrentRunIsUnknown() async throws {
        let counter = Counter()
        let envelope = { (seq: Int) in
            "id: \(seq)\ndata: {\"seq\":\(seq),\"at\":\"2026-09-27T10:00:00Z\",\"event\":{\"type\":\"phase_started\",\"run\":\"run-a\",\"phase\":\"qa\"}}\n\n"
        }
        StubProtocol.install { _ in
            let n = counter.next()
            let body = n == 0 ? envelope(1) + envelope(2) : envelope(1) + envelope(2) + envelope(3)
            return StubProtocol.Reply(status: 200, chunks: n < 2 ? [Data(body.utf8)] : [])
        }
        struct Unreachable: Error {}
        let stream = EventStream(endpoint: endpoint, path: "tasks/3/stream", session: StubProtocol.session(),
                                 backoff: Backoff(initial: 0.01, maximum: 0.01)) { throw Unreachable() }
        var seqs: [UInt64?] = []
        for try await envelope in stream.envelopes() {
            seqs.append(envelope.seq)
            if seqs.count == 3 { break }
        }
        // Asked from the start, and the replayed 1 and 2 were dropped.
        XCTAssertEqual(seqs, [1, 2, 3])
        XCTAssertNil(StubProtocol.requests[1].query("after"))
    }

    func testStreamRetriesAfterAServerError() async throws {
        let counter = Counter()
        StubProtocol.install { _ in
            if counter.next() == 0 { return self.json(#"{"error":"busy"}"#, status: 503) }
            let body = "id: 1\ndata: {\"seq\":1,\"at\":\"2026-09-27T10:00:00Z\",\"event\":{\"type\":\"paused\",\"run\":\"r\",\"reason\":\"x\"}}\n\n"
            return StubProtocol.Reply(status: 200, chunks: [Data(body.utf8)])
        }
        let stream = EventStream(endpoint: endpoint, path: "tasks/3/stream", session: StubProtocol.session(),
                                 backoff: Backoff(initial: 0.01, maximum: 0.01))
        var types: [String] = []
        for try await envelope in stream.envelopes() {
            types.append(envelope.event.typeName)
            break
        }
        XCTAssertEqual(types, ["paused"])
        XCTAssertGreaterThanOrEqual(StubProtocol.requests.count, 2)
    }

    func testStreamStopsOnUnauthorized() async throws {
        StubProtocol.install { _ in self.json(#"{"error":"missing or wrong token"}"#, status: 401) }
        let stream = EventStream(endpoint: endpoint, path: "tasks/3/stream", session: StubProtocol.session(),
                                 backoff: Backoff(initial: 0.01, maximum: 0.01))
        do {
            for try await _ in stream.envelopes() {}
            XCTFail("expected an error")
        } catch let error as VibeError {
            XCTAssertEqual(error, .unauthorized)
        }
    }

    private func assertThrows(_ expected: VibeError, _ body: () async throws -> Void,
                              file: StaticString = #filePath, line: UInt = #line) async {
        do {
            try await body()
            XCTFail("expected \(expected)", file: file, line: line)
        } catch let error as VibeError {
            XCTAssertEqual(error, expected, file: file, line: line)
        } catch {
            XCTFail("unexpected \(error)", file: file, line: line)
        }
    }
}

final class Counter: @unchecked Sendable {
    private let lock = NSLock()
    private var value = 0

    func next() -> Int {
        lock.withLock {
            defer { value += 1 }
            return value
        }
    }
}

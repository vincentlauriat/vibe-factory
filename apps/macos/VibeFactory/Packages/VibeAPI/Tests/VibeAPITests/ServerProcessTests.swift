import XCTest
@testable import VibeAPI

final class ServerProcessTests: XCTestCase {
    func testAnnouncementLine() throws {
        let json = try ServerProcess.parseAnnouncement(#"{"port":58566,"token":"abc","url":"http://127.0.0.1:58566/"}"#)
        XCTAssertEqual(json, ServerEndpoint(baseURL: URL(string: "http://127.0.0.1:58566/")!, token: "abc"))

        // Anything else is refused, and the message never repeats the text (it may hold the token).
        let token = String(repeating: "ab", count: 32)
        XCTAssertThrowsError(try ServerProcess.parseAnnouncement("API token: \(token)")) { error in
            XCTAssertFalse(error.localizedDescription.contains(token))
        }
    }

    func testTokensAreRedacted() {
        let token = String(repeating: "0f", count: 32)
        XCTAssertEqual(ServerProcess.redact("open http://x/#token=\(token) now"), "open http://x/#token=‹token› now")
    }

    func testArgumentsNeverGoThroughAShell() {
        let server = ServerProcess(project: URL(fileURLWithPath: "/tmp/my project"),
                                   executable: URL(fileURLWithPath: "/usr/bin/false"),
                                   evals: URL(fileURLWithPath: "/tmp/evals"))
        XCTAssertEqual(server.arguments, [
            "--project", "/tmp/my project", "--json", "serve", "--bind", "127.0.0.1", "--port", "0",
            "--exit-on-stdin-eof", "--evals", "/tmp/evals",
        ])
    }

    func testExecutableResolution() throws {
        XCTAssertNil(ServerProcess.resolveExecutable(setting: "/nonexistent/vibe"))
        XCTAssertEqual(ServerProcess.resolveExecutable(setting: "/bin/ls")?.path, "/bin/ls")
        let environment = ServerProcess.childEnvironment()
        XCTAssertTrue(environment["PATH"]?.contains(".cargo/bin") ?? false)
    }

    func testTheChildEnvironmentGetsTheAppKeys() {
        let environment = ServerProcess.childEnvironment(adding: ["ANTHROPIC_API_KEY": "sk-test", "": "x"])
        XCTAssertEqual(environment["ANTHROPIC_API_KEY"], "sk-test")
        XCTAssertNil(environment[""])
    }

    /// A child that never announces itself is stopped when `start()` gives up.
    func testAStartThatTimesOutStopsTheChild() async throws {
        let script = try makeScript("sleep 30")
        let server = ServerProcess(project: URL(fileURLWithPath: NSTemporaryDirectory()), executable: script)
        do {
            _ = try await server.start(timeout: 1)
            XCTFail("expected a timeout")
        } catch ServerProcessError.notReady {
        }
        XCTAssertGreaterThan(server.pid, 0)
        XCTAssertFalse(server.isRunning)
    }

    /// Once started, a child that dies is reported through `onExit`; `stop()` is not.
    func testOnExitReportsADeathAfterStart() async throws {
        // A fake server: announces itself, answers /api/health from python, then dies.
        let port = Int.random(in: 40_000..<60_000)
        let script = try makeScript("""
            echo '{"port":\(port),"token":"t","url":"http://127.0.0.1:\(port)/"}'
            echo 'failing with token \(String(repeating: "ab", count: 32))' >&2
            exec /usr/bin/python3 -c '
            import http.server, sys
            class H(http.server.BaseHTTPRequestHandler):
                def do_GET(self):
                    body = b"{\\"name\\":\\"vibe\\",\\"version\\":\\"test\\"}"
                    self.send_response(200); self.send_header("Content-Length", str(len(body))); self.end_headers()
                    self.wfile.write(body)
                    sys.exit(3)
                def log_message(self, *a): pass
            http.server.HTTPServer(("127.0.0.1", \(port)), H).handle_request()
            sys.exit(3)
            '
            """)
        let server = ServerProcess(project: URL(fileURLWithPath: NSTemporaryDirectory()), executable: script)
        let exited = expectation(description: "onExit")
        let seen = Counter()
        server.onExit = { status, output in
            XCTAssertEqual(status, 3)
            XCTAssertTrue(output.contains("‹token›"))
            _ = seen.next()
            exited.fulfill()
        }
        _ = try await server.start(timeout: 10)
        await fulfillment(of: [exited], timeout: 10)
        XCTAssertEqual(seen.next(), 1)
    }

    private func makeScript(_ body: String) throws -> URL {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("vibe-fake-\(UUID().uuidString).sh")
        try ("#!/bin/sh\n" + body + "\n").write(to: url, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: url.path)
        addTeardownBlock { try? FileManager.default.removeItem(at: url) }
        return url
    }

    func testAChildThatExitsEarlyReportsItsStatus() async throws {
        let server = ServerProcess(project: URL(fileURLWithPath: NSTemporaryDirectory()),
                                   executable: URL(fileURLWithPath: "/usr/bin/false"))
        do {
            _ = try await server.start(timeout: 5)
            XCTFail("expected an error")
        } catch ServerProcessError.exitedEarly(let status, _) {
            XCTAssertEqual(status, 1)
        }
    }

    /// Starts the real `vibe serve` on this repository. Opt-in, and needs
    /// vibe ≥ 0.5 (`--exit-on-stdin-eof`):
    /// `VIBE_SMOKE=1 swift test --filter ServerProcessTests`; `VIBE_EXECUTABLE`
    /// picks the binary (`<repo>/target/debug/vibe` for the checkout).
    func testSmokeAgainstThisRepository() async throws {
        let environment = ProcessInfo.processInfo.environment
        try XCTSkipUnless(environment["VIBE_SMOKE"] == "1", "set VIBE_SMOKE=1")
        let executable = try XCTUnwrap(ServerProcess.resolveExecutable(setting: environment["VIBE_EXECUTABLE"]),
                                       "no vibe executable")
        // Tests/VibeAPITests/ → Packages/VibeAPI → apps/macos/VibeFactory → repository root.
        var root = URL(fileURLWithPath: #filePath)
        for _ in 0..<8 { root.deleteLastPathComponent() }
        XCTAssertTrue(FileManager.default.fileExists(atPath: root.appendingPathComponent("Cargo.toml").path))

        let server = ServerProcess(project: root, executable: executable)
        let endpoint: ServerEndpoint
        do {
            endpoint = try await server.start()
        } catch {
            // Caught here so the report names the real error (thrown out of an
            // async test, XCTest reports a CancellationError instead).
            return XCTFail("vibe serve did not start (requires vibe ≥ 0.5): \(error.localizedDescription)")
        }
        let client = VibeClient(endpoint: endpoint)
        let health = try await client.health()
        XCTAssertEqual(health.name, "vibe")
        let tasks = try await client.tasks()
        let evals = try await client.evals()
        XCTAssertFalse(evals.enabled)
        await assertUnauthorized(VibeClient(endpoint: ServerEndpoint(baseURL: endpoint.baseURL, token: "wrong")))

        // The project-wide routes of 0.5.
        let history = try await client.history(all: true)
        let page = try await client.events(limit: 50)
        let streamStatus = try await openStream(endpoint, after: page.cursor)
        XCTAssertEqual(streamStatus.status, 200)
        XCTAssertTrue(streamStatus.contentType.hasPrefix("text/event-stream"), streamStatus.contentType)
        print("smoke: vibe \(health.version) on \(endpoint.baseURL), \(tasks.count) task(s), "
              + "\(history.count) in history, \(page.events.count) event(s), cursor \(page.cursor?.description ?? "none"), "
              + "read errors \(page.readErrors), stream \(streamStatus.status) \(streamStatus.contentType), pid \(server.pid)")

        await server.stop(grace: 10)
        XCTAssertFalse(server.isRunning)
    }

    /// Open `/api/stream` and read its headers only. URLSession hands the
    /// response over with the first bytes of the body, and a quiet project
    /// sends nothing but a keep-alive comment every 15 s: allow for one.
    private func openStream(_ endpoint: ServerEndpoint, after: EventCursor?) async throws -> (status: Int, contentType: String) {
        let query = after.map { [URLQueryItem(name: "after", value: $0.description)] } ?? []
        var request = endpoint.request("stream", query: query)
        request.timeoutInterval = 25
        let (bytes, response) = try await URLSession.shared.bytes(for: request)
        bytes.task.cancel()
        let http = try XCTUnwrap(response as? HTTPURLResponse)
        return (http.statusCode, http.value(forHTTPHeaderField: "Content-Type") ?? "")
    }

    private func assertUnauthorized(_ client: VibeClient) async {
        do {
            _ = try await client.tasks()
            XCTFail("expected 401")
        } catch {
            XCTAssertEqual(error as? VibeError, .unauthorized)
        }
    }
}

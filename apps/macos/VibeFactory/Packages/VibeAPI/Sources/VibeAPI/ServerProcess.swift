import Foundation

/// Why a `vibe serve` child could not be started.
public enum ServerProcessError: Error, Equatable, LocalizedError {
    /// No `vibe` executable in the setting, `~/.cargo/bin` or `PATH`.
    case executableNotFound
    /// The process ended before it was ready; `output` is the end of its stderr.
    case exitedEarly(status: Int32, output: String)
    /// It printed nothing usable, or did not answer `/api/health` in time.
    case notReady(String)

    public var errorDescription: String? {
        switch self {
        case .executableNotFound:
            "The vibe executable was not found (Settings, ~/.cargo/bin, PATH)."
        case .exitedEarly(let status, let output):
            "vibe serve exited with status \(status).\n\(output)"
        case .notReady(let message):
            message
        }
    }
}

/// A `vibe serve` child process for one project.
///
/// Started as `vibe --project <dir> --json serve --bind 127.0.0.1 --port 0
/// --exit-on-stdin-eof`, never through a shell (requires vibe ≥ 0.5):
/// - the server picks a free port and prints `{"url", "port", "token"}` on
///   its first stdout line. It also writes the token to `.vibe/server.token`,
///   which another server of the same project could overwrite, so the line is
///   preferred. The endpoint is ready once `/api/health` answers;
/// - its stdin is a pipe the app never writes to and keeps open: when the app
///   dies, even by SIGKILL or a crash, the pipe closes and the server shuts
///   down gracefully instead of staying orphaned;
/// - `stop()` sends SIGINT, the server's graceful shutdown (running runs are
///   cancelled and stay resumable), then SIGTERM after a grace period;
/// - `onExit` reports a child that ends on its own after `start()`.
public final class ServerProcess: @unchecked Sendable {
    public let project: URL
    public let executable: URL
    public let evals: URL?
    /// Variables added to the child's environment (API keys kept by the app).
    public let extraEnvironment: [String: String]

    /// Called once, from a background thread, when the child ends after a
    /// successful `start()` without `stop()`/`interruptAndWait()` having been
    /// asked: the exit status and the end of stderr (tokens redacted).
    public var onExit: (@Sendable (Int32, String) -> Void)? {
        get { lock.withLock { _onExit } }
        set { lock.withLock { _onExit = newValue } }
    }

    private let lock = NSLock()
    private let process = Process()
    /// Kept open for the life of the child (see `--exit-on-stdin-eof`).
    private let stdinPipe = Pipe()
    private var stdoutBuffer = Data()
    private var stderrTail = Data()
    private var firstLine: String?
    private var lineWaiter: CheckedContinuation<String, Error>?
    private var _endpoint: ServerEndpoint?
    private var _onExit: (@Sendable (Int32, String) -> Void)?
    private var stopping = false

    /// Longest stderr kept for error messages.
    private static let tailLimit = 8 * 1024

    public init(project: URL, executable: URL, evals: URL? = nil, extraEnvironment: [String: String] = [:]) {
        self.project = project
        self.executable = executable
        self.evals = evals
        self.extraEnvironment = extraEnvironment
    }

    deinit {
        if process.isRunning { process.interrupt() }
    }

    /// Where the server listens, once started.
    public var endpoint: ServerEndpoint? {
        lock.withLock { _endpoint }
    }

    public var isRunning: Bool { process.isRunning }

    /// Process id of the child, 0 before `start()`.
    public var pid: Int32 { process.processIdentifier }

    /// The last lines the server wrote to stderr, tokens redacted.
    public var errorOutput: String {
        Self.redact(lock.withLock { String(decoding: stderrTail, as: UTF8.self) })
    }

    /// The arguments passed to `vibe`.
    public var arguments: [String] {
        var arguments = ["--project", project.path, "--json", "serve", "--bind", "127.0.0.1", "--port", "0",
                         "--exit-on-stdin-eof"]
        if let evals { arguments += ["--evals", evals.path] }
        return arguments
    }

    /// Start the child and wait until it answers `/api/health`. On failure
    /// (including the timeout) the child is stopped before the error is thrown.
    public func start(timeout: TimeInterval = 30) async throws -> ServerEndpoint {
        process.executableURL = executable
        process.arguments = arguments
        process.currentDirectoryURL = project
        process.environment = Self.childEnvironment(adding: extraEnvironment)
        let stdout = Pipe()
        let stderr = Pipe()
        process.standardOutput = stdout
        process.standardError = stderr
        process.standardInput = stdinPipe
        // Both pipes are drained for the life of the child: a full pipe
        // would block the server.
        stdout.fileHandleForReading.readabilityHandler = { [weak self] handle in
            self?.receiveStdout(handle.availableData)
        }
        stderr.fileHandleForReading.readabilityHandler = { [weak self] handle in
            self?.receiveStderr(handle.availableData)
        }
        process.terminationHandler = { [weak self] process in
            stdout.fileHandleForReading.readabilityHandler = nil
            stderr.fileHandleForReading.readabilityHandler = nil
            self?.childExited(status: process.terminationStatus)
        }
        do {
            try process.run()
        } catch {
            throw ServerProcessError.notReady("cannot start \(executable.path): \(error.localizedDescription)")
        }

        do {
            let deadline = Date().addingTimeInterval(timeout)
            let line = try await firstStdoutLine(timeout: timeout)
            let endpoint = try Self.parseAnnouncement(line)
            try await waitForHealth(endpoint, until: deadline)
            lock.withLock { _endpoint = endpoint }
            return endpoint
        } catch {
            await stop(grace: 3)
            throw error
        }
    }

    /// Ask the server to stop (SIGINT) and wait up to `grace` seconds for it
    /// to cancel its runs, then terminate it.
    public func stop(grace: TimeInterval = 35) async {
        lock.withLock { stopping = true }
        guard process.isRunning else { return }
        process.interrupt()
        let deadline = Date().addingTimeInterval(grace)
        while process.isRunning, Date() < deadline {
            try? await Task.sleep(nanoseconds: 100_000_000)
        }
        if process.isRunning { process.terminate() }
    }

    /// SIGINT without waiting, for application termination (the caller waits
    /// once for every child).
    public func interrupt() {
        lock.withLock { stopping = true }
        if process.isRunning { process.interrupt() }
    }

    /// SIGINT and a short synchronous wait.
    public func interruptAndWait(upTo seconds: TimeInterval = 2) {
        interrupt()
        let deadline = Date().addingTimeInterval(seconds)
        while process.isRunning, Date() < deadline {
            Thread.sleep(forTimeInterval: 0.05)
        }
    }

    // MARK: Executable and environment

    /// Directories searched after the user setting; a Finder-launched app has
    /// a minimal `PATH`.
    static var extraPath: [String] {
        let home = FileManager.default.homeDirectoryForCurrentUser.path
        return ["\(home)/.cargo/bin", "/opt/homebrew/bin", "/usr/local/bin"]
    }

    /// The `vibe` to run: the user's setting when it is executable, then
    /// `~/.cargo/bin/vibe`, then the first `vibe` of `PATH`.
    public static func resolveExecutable(setting: String?, environment: [String: String] = ProcessInfo.processInfo.environment) -> URL? {
        let fm = FileManager.default
        if let setting = setting?.trimmingCharacters(in: .whitespaces), !setting.isEmpty {
            let path = (setting as NSString).expandingTildeInPath
            return fm.isExecutableFile(atPath: path) ? URL(fileURLWithPath: path) : nil
        }
        let cargo = fm.homeDirectoryForCurrentUser.appendingPathComponent(".cargo/bin/vibe")
        if fm.isExecutableFile(atPath: cargo.path) { return cargo }
        let path = (environment["PATH"] ?? "").split(separator: ":").map(String.init) + extraPath
        for directory in path where !directory.isEmpty {
            let candidate = URL(fileURLWithPath: directory).appendingPathComponent("vibe")
            if fm.isExecutableFile(atPath: candidate.path) { return candidate }
        }
        return nil
    }

    /// The app's environment with the usual tool directories added to `PATH`
    /// (git and provider CLIs must resolve for the agents) and `extra` set
    /// (a Finder-launched app does not see variables exported by the shell).
    static func childEnvironment(adding extra: [String: String] = [:]) -> [String: String] {
        var environment = ProcessInfo.processInfo.environment
        var path = (environment["PATH"] ?? "/usr/bin:/bin:/usr/sbin:/sbin").split(separator: ":").map(String.init)
        for directory in extraPath where !path.contains(directory) {
            path.append(directory)
        }
        environment["PATH"] = path.joined(separator: ":")
        for (name, value) in extra where !name.isEmpty {
            environment[name] = value
        }
        return environment
    }

    // MARK: Output

    /// The first stdout line of `vibe --json serve`: `{"url","port","token"}`.
    static func parseAnnouncement(_ text: String) throws -> ServerEndpoint {
        guard let data = text.data(using: .utf8),
              let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let token = object["token"] as? String,
              let port = object["port"] as? Int else {
            // The text may hold the token: never put it in the message.
            throw ServerProcessError.notReady(
                "unexpected first line from vibe serve (\(text.utf8.count) bytes); vibe ≥ 0.5 is required")
        }
        // The Host check accepts 127.0.0.1:<port>; build the URL on it.
        return ServerEndpoint(baseURL: URL(string: "http://127.0.0.1:\(port)/")!, token: token)
    }

    /// Replace anything shaped like a server token (64 hex digits) by `‹token›`.
    static func redact(_ text: String) -> String {
        text.replacingOccurrences(of: "[0-9a-fA-F]{64}", with: "‹token›", options: .regularExpression)
    }

    private func childExited(status: Int32) {
        let (waiter, callback, output) = lock.withLock {
            () -> (CheckedContinuation<String, Error>?, (@Sendable (Int32, String) -> Void)?, String) in
            let waiter = lineWaiter
            lineWaiter = nil
            let callback = (_endpoint != nil && !stopping) ? _onExit : nil
            _onExit = nil
            return (waiter, callback, String(decoding: stderrTail, as: UTF8.self))
        }
        let redacted = Self.redact(output)
        waiter?.resume(throwing: ServerProcessError.exitedEarly(status: status, output: redacted))
        callback?(status, redacted)
    }

    private func receiveStdout(_ data: Data) {
        guard !data.isEmpty else { return }
        let waiter: CheckedContinuation<String, Error>?
        let line: String?
        lock.lock()
        if firstLine == nil {
            stdoutBuffer.append(data)
            if let newline = stdoutBuffer.firstIndex(of: 0x0A) {
                firstLine = String(decoding: stdoutBuffer[..<newline], as: UTF8.self)
                stdoutBuffer.removeAll()
            }
        }
        line = firstLine
        waiter = line == nil ? nil : lineWaiter
        if line != nil { lineWaiter = nil }
        lock.unlock()
        if let waiter, let line { waiter.resume(returning: line) }
    }

    private func receiveStderr(_ data: Data) {
        guard !data.isEmpty else { return }
        lock.withLock {
            stderrTail.append(data)
            if stderrTail.count > Self.tailLimit {
                stderrTail.removeFirst(stderrTail.count - Self.tailLimit)
            }
        }
    }

    private func failWaiter(_ error: Error) {
        let waiter = lock.withLock { () -> CheckedContinuation<String, Error>? in
            defer { lineWaiter = nil }
            return lineWaiter
        }
        waiter?.resume(throwing: error)
    }

    private func firstStdoutLine(timeout: TimeInterval) async throws -> String {
        // A timer that never throws: cancelling it must not surface as an error.
        let timer = Task { [weak self] in
            try? await Task.sleep(nanoseconds: UInt64(timeout * 1_000_000_000))
            guard !Task.isCancelled else { return }
            self?.failWaiter(ServerProcessError.notReady("vibe serve printed nothing in \(Int(timeout)) s"))
        }
        defer { timer.cancel() }
        return try await withCheckedThrowingContinuation { continuation in
            lock.lock()
            if let firstLine {
                lock.unlock()
                continuation.resume(returning: firstLine)
            } else if !process.isRunning {
                let output = Self.redact(String(decoding: stderrTail, as: UTF8.self))
                lock.unlock()
                continuation.resume(throwing: ServerProcessError.exitedEarly(
                    status: process.terminationStatus, output: output))
            } else {
                lineWaiter = continuation
                lock.unlock()
            }
        }
    }

    private func waitForHealth(_ endpoint: ServerEndpoint, until deadline: Date) async throws {
        let client = VibeClient(endpoint: endpoint)
        var lastError: Error?
        while Date() < deadline {
            guard process.isRunning else {
                throw ServerProcessError.exitedEarly(status: process.terminationStatus, output: errorOutput)
            }
            do {
                _ = try await client.health()
                return
            } catch {
                lastError = error
                try await Task.sleep(nanoseconds: 150_000_000)
            }
        }
        throw ServerProcessError.notReady("vibe serve did not answer: \(lastError?.localizedDescription ?? "timeout")")
    }
}

import Foundation

/// One server-sent event.
public struct SSEMessage: Hashable, Sendable {
    /// The `event:` field, `message` when absent.
    public var event: String
    /// The `data:` lines joined by `\n`.
    public var data: String
    /// The last event id at dispatch time (the `id:` of this message or an earlier one).
    public var lastEventId: String
    /// The `id:` of this very message; `nil` when it had none (the global
    /// stream's `agent_delta`), even though `lastEventId` is kept.
    public var id: String?

    public init(event: String = "message", data: String, lastEventId: String = "", id: String? = nil) {
        self.event = event
        self.data = data
        self.lastEventId = lastEventId
        self.id = id
    }
}

/// Incremental parser of a `text/event-stream` body, byte by byte, following
/// the HTML specification: lines end with CRLF, LF or CR (also when a chunk
/// boundary falls between CR and LF), `:` starts a comment, `data:` lines
/// accumulate, a blank line dispatches, an event without data is dropped but
/// its `id:` still counts. An `id:` takes effect only when its event is
/// dispatched (at the blank line): an event cut off by the end of the body
/// does not move `lastEventId`. Independent of the payload: it serves the task
/// streams and the global stream alike.
public struct SSEParser: Sendable {
    public private(set) var lastEventId = ""
    private var line: [UInt8] = []
    private var afterCR = false
    private var eventType = ""
    private var pendingId: String?
    private var data = ""
    private var hasData = false

    public init(lastEventId: String = "") {
        self.lastEventId = lastEventId
    }

    /// Feed one byte; returns a message when it completes one.
    public mutating func push(_ byte: UInt8) -> SSEMessage? {
        if afterCR {
            afterCR = false
            if byte == 0x0A { return nil }
        }
        switch byte {
        case 0x0D:
            afterCR = true
            return endLine()
        case 0x0A:
            return endLine()
        default:
            line.append(byte)
            return nil
        }
    }

    /// Feed a chunk; returns the messages it completes.
    public mutating func push<S: Sequence>(_ bytes: S) -> [SSEMessage] where S.Element == UInt8 {
        bytes.compactMap { push($0) }
    }

    /// End of the body: a trailing event without its blank line is discarded
    /// (as the specification requires); the partial state is reset.
    public mutating func finish() {
        line.removeAll()
        afterCR = false
        eventType = ""
        data = ""
        hasData = false
        pendingId = nil
    }

    private mutating func endLine() -> SSEMessage? {
        defer { line.removeAll(keepingCapacity: true) }
        if line.isEmpty { return dispatch() }
        let text = String(decoding: line, as: UTF8.self)
        if text.hasPrefix(":") { return nil }
        let field: Substring
        var value: Substring
        if let colon = text.firstIndex(of: ":") {
            field = text[..<colon]
            value = text[text.index(after: colon)...]
            if value.hasPrefix(" ") { value = value.dropFirst() }
        } else {
            field = Substring(text)
            value = ""
        }
        switch field {
        case "event":
            eventType = String(value)
        case "data":
            if hasData { data.append("\n") }
            data.append(contentsOf: value)
            hasData = true
        case "id":
            if !value.contains("\0") { pendingId = String(value) }
        default:
            break // `retry` and unknown fields: the client has its own backoff.
        }
        return nil
    }

    private mutating func dispatch() -> SSEMessage? {
        if let pendingId { lastEventId = pendingId }
        defer {
            pendingId = nil
            eventType = ""
            data = ""
            hasData = false
        }
        guard hasData else { return nil }
        return SSEMessage(event: eventType.isEmpty ? "message" : eventType, data: data, lastEventId: lastEventId,
                          id: pendingId)
    }
}

/// Reconnection delays: doubled after each failed attempt, capped. Any 503
/// waits longer (from `vibe serve`, the limit of open streams, `MAX_STREAMS`
/// in `crates/vibe-cli/src/server/api/read.rs`): a saturated server is not
/// asked again every few seconds.
public struct Backoff: Sendable {
    public var initial: TimeInterval
    public var maximum: TimeInterval
    /// The same after a 503.
    public var saturatedInitial: TimeInterval
    public var saturatedMaximum: TimeInterval
    /// Waits between attempts; tests replace it to record the delays.
    public var sleep: @Sendable (TimeInterval) async -> Void

    public init(initial: TimeInterval = 0.5, maximum: TimeInterval = 10, saturatedInitial: TimeInterval = 5,
                saturatedMaximum: TimeInterval = 60,
                sleep: @escaping @Sendable (TimeInterval) async -> Void = Backoff.taskSleep) {
        self.initial = initial
        self.maximum = maximum
        self.saturatedInitial = saturatedInitial
        self.saturatedMaximum = saturatedMaximum
        self.sleep = sleep
    }

    public func delay(attempt: Int, saturated: Bool = false) -> TimeInterval {
        let (start, cap) = saturated ? (saturatedInitial, saturatedMaximum) : (initial, maximum)
        return min(cap, start * pow(2, Double(max(0, attempt))))
    }

    public static let taskSleep: @Sendable (TimeInterval) async -> Void = { seconds in
        try? await Task.sleep(nanoseconds: UInt64(seconds * 1_000_000_000))
    }
}

/// An SSE connection that reconnects with backoff. The caller builds each
/// request from the last event id seen, so the same connection serves the
/// task streams (`after=<seq>`) and the global stream (`after=<cursor>`).
public struct SSEConnection: Sendable {
    public typealias RequestBuilder = @Sendable (_ lastEventId: String) async throws -> URLRequest

    private let session: URLSession
    private let backoff: Backoff
    private let makeRequest: RequestBuilder

    public init(session: URLSession = .shared, backoff: Backoff = Backoff(), makeRequest: @escaping RequestBuilder) {
        self.session = session
        self.backoff = backoff
        self.makeRequest = makeRequest
    }

    /// Messages until the consumer stops iterating. Fails only on answers a
    /// retry cannot fix (401, 404, other 4xx); network errors, 5xx and a
    /// closed body reconnect, a 503 with the longer delays. The count of
    /// failed attempts restarts when a message is delivered or when the kind
    /// of failure changes (network errors then a first 503 wait 5 s, not 60).
    public func messages(lastEventId: String = "") -> AsyncThrowingStream<SSEMessage, Error> {
        let session = session
        let backoff = backoff
        let makeRequest = makeRequest
        return AsyncThrowingStream { continuation in
            let task = Task {
                var lastId = lastEventId
                var failures = 0
                var lastSaturated = false
                while !Task.isCancelled {
                    var saturated = false
                    do {
                        var request = try await makeRequest(lastId)
                        request.setValue("text/event-stream", forHTTPHeaderField: "Accept")
                        request.setValue(lastId.isEmpty ? nil : lastId, forHTTPHeaderField: "Last-Event-ID")
                        let (bytes, response) = try await session.bytes(for: request)
                        if let http = response as? HTTPURLResponse, !(200..<300).contains(http.statusCode) {
                            var body = Data()
                            for try await byte in bytes { body.append(byte) }
                            try VibeClient.check(response, body: body)
                        }
                        var parser = SSEParser(lastEventId: lastId)
                        for try await byte in bytes {
                            if let message = parser.push(byte) {
                                failures = 0
                                lastId = message.lastEventId
                                continuation.yield(message)
                            }
                        }
                        lastId = parser.lastEventId
                    } catch let error as VibeError {
                        if case .server(let status, _) = error, status >= 500 {
                            saturated = status == 503 // retry below
                        } else if case .transport = error {
                            // Retry below.
                        } else {
                            continuation.finish(throwing: error)
                            return
                        }
                    } catch {
                        if Task.isCancelled { break }
                    }
                    if saturated != lastSaturated { failures = 0 }
                    lastSaturated = saturated
                    let delay = backoff.delay(attempt: failures, saturated: saturated)
                    failures += 1
                    await backoff.sleep(delay)
                }
                continuation.finish()
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }
}

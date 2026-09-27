import Foundation

/// Live events of one task: `GET /api/tasks/{ref}/stream`.
///
/// The server sends the logged events of the task's current run after
/// `after`, then new ones, plus the ephemeral `agent_delta` of runs it
/// started itself (without `id:`). `seq` restarts at 1 with each run, so the
/// stream remembers the run of the last envelope with its `seq`:
/// - envelopes already seen (same run, `seq` not greater) are dropped;
/// - on reconnection it asks for `after=<seq>` if the task's current run is
///   still the one it saw, and from the start (`after=0`) otherwise, so that
///   the first events of a run started while it was disconnected are not
///   skipped. When the current run cannot be known (the request fails), it
///   also asks from the start: the `(run, seq)` check drops the replayed part.
public struct EventStream: Sendable {
    private let endpoint: ServerEndpoint
    private let path: String
    private let after: UInt64
    private let session: URLSession
    private let backoff: Backoff
    private let currentRun: (@Sendable () async throws -> String?)?

    /// - Parameter currentRun: the run id of the task's `run.json`, asked
    ///   before each reconnection; `nil` keeps the last `seq`.
    public init(endpoint: ServerEndpoint, path: String, after: UInt64 = 0, session: URLSession = .shared,
                backoff: Backoff = Backoff(), currentRun: (@Sendable () async throws -> String?)? = nil) {
        self.endpoint = endpoint
        self.path = path
        self.after = after
        self.session = session
        self.backoff = backoff
        self.currentRun = currentRun
    }

    /// Envelopes until the consumer stops iterating (or on 401/404).
    public func envelopes() -> AsyncThrowingStream<Envelope, Error> {
        let cursor = Cursor(seq: after)
        let endpoint = endpoint
        let path = path
        let currentRun = currentRun
        let connection = SSEConnection(session: session, backoff: backoff) { _ in
            var after = await cursor.seq
            if after > 0, let seen = await cursor.run, let currentRun {
                do {
                    if try await currentRun() != seen {
                        after = 0
                        await cursor.reset()
                    }
                } catch {
                    after = 0 // replay; the cursor drops what was already delivered
                }
            }
            let query = after > 0 ? [URLQueryItem(name: "after", value: String(after))] : []
            return endpoint.request(path, query: query)
        }
        let messages = connection.messages()
        return AsyncThrowingStream { continuation in
            let task = Task {
                let decoder = VibeJSON.decoder()
                do {
                    for try await message in messages {
                        guard let envelope = try? decoder.decode(Envelope.self, from: Data(message.data.utf8)) else {
                            continue
                        }
                        if await cursor.accept(envelope) {
                            continuation.yield(envelope)
                        }
                    }
                    continuation.finish()
                } catch {
                    continuation.finish(throwing: error)
                }
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }

    /// Run and `seq` of the last logged envelope delivered.
    actor Cursor {
        var run: String?
        var seq: UInt64

        init(seq: UInt64) { self.seq = seq }

        func reset() {
            run = nil
            seq = 0
        }

        /// Whether to deliver the envelope; records its position.
        func accept(_ envelope: Envelope) -> Bool {
            guard let next = envelope.seq else { return true } // ephemeral
            let envelopeRun = envelope.event.runId
            if envelopeRun == run, next <= seq { return false }
            run = envelopeRun
            seq = next
            return true
        }
    }
}

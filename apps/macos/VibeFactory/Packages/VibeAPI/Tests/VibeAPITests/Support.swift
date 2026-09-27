import Foundation
import XCTest
@testable import VibeAPI

enum Fixture {
    static func data(_ name: String) throws -> Data {
        let url = try XCTUnwrap(Bundle.module.url(forResource: "Fixtures/\(name)", withExtension: nil))
        return try Data(contentsOf: url)
    }

    static func decode<T: Decodable>(_ type: T.Type, _ name: String) throws -> T {
        try VibeJSON.decoder().decode(T.self, from: data(name))
    }

    static func envelopes(_ name: String) throws -> [Envelope] {
        let text = String(decoding: try data(name), as: UTF8.self)
        let decoder = VibeJSON.decoder()
        return try text.split(separator: "\n").map { try decoder.decode(Envelope.self, from: Data($0.utf8)) }
    }
}

/// A `URLProtocol` answering from a handler, recording every request.
final class StubProtocol: URLProtocol {
    struct Reply {
        var status: Int
        var headers: [String: String] = ["Content-Type": "application/json"]
        var chunks: [Data]
        /// Never finish the response (to test cancellation).
        var hang = false
    }

    private static let lock = NSLock()
    nonisolated(unsafe) private static var handler: ((URLRequest) -> Reply)?
    nonisolated(unsafe) private static var recorded: [URLRequest] = []

    static func install(_ handler: @escaping (URLRequest) -> Reply) {
        lock.withLock {
            self.handler = handler
            recorded = []
        }
    }

    static var requests: [URLRequest] { lock.withLock { recorded } }

    static func session() -> URLSession {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [StubProtocol.self]
        return URLSession(configuration: configuration)
    }

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        var request = request
        // URLSession moves bodies to a stream; keep them readable for assertions.
        if request.httpBody == nil, let stream = request.httpBodyStream {
            stream.open()
            var body = Data()
            var buffer = [UInt8](repeating: 0, count: 4096)
            while stream.hasBytesAvailable {
                let count = stream.read(&buffer, maxLength: buffer.count)
                if count <= 0 { break }
                body.append(buffer, count: count)
            }
            stream.close()
            request.httpBody = body
        }
        let reply = Self.lock.withLock { () -> Reply? in
            Self.recorded.append(request)
            return Self.handler?(request)
        } ?? Reply(status: 500, chunks: [])
        let response = HTTPURLResponse(url: request.url!, statusCode: reply.status, httpVersion: "HTTP/1.1",
                                       headerFields: reply.headers)!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        for chunk in reply.chunks {
            client?.urlProtocol(self, didLoad: chunk)
        }
        if !reply.hang { client?.urlProtocolDidFinishLoading(self) }
    }

    override func stopLoading() {}
}

extension URLRequest {
    var bodyJSON: [String: Any]? {
        httpBody.flatMap { try? JSONSerialization.jsonObject(with: $0) as? [String: Any] }
    }

    func query(_ name: String) -> String? {
        url.flatMap { URLComponents(url: $0, resolvingAgainstBaseURL: false) }?
            .queryItems?.first { $0.name == name }?.value
    }
}

import XCTest
@testable import VibeAPI

final class SSEParserTests: XCTestCase {
    private func parse(_ chunks: [String], parser: inout SSEParser) -> [SSEMessage] {
        chunks.flatMap { parser.push(Array($0.utf8)) }
    }

    private func parse(_ chunks: [String]) -> [SSEMessage] {
        var parser = SSEParser()
        return parse(chunks, parser: &parser)
    }

    func testTheShapeAxumSends() {
        let messages = parse(["event: phase_started\nid: 2\ndata: {\"seq\":2}\n\n: keep-alive\n\n"])
        XCTAssertEqual(messages, [SSEMessage(event: "phase_started", data: #"{"seq":2}"#, lastEventId: "2", id: "2")])
    }

    func testChunkBoundariesAnywhere() {
        let body = "event: a\nid: 7\ndata: héllo wörld\n\nevent: b\ndata: x\n\n"
        let bytes = Array(body.utf8)
        let expected = parse([body])
        XCTAssertEqual(expected.count, 2)
        // Every split point, including inside a multi-byte character.
        for cut in 1..<bytes.count {
            var parser = SSEParser()
            let messages = parser.push(bytes[..<cut]) + parser.push(bytes[cut...])
            XCTAssertEqual(messages, expected, "split at \(cut)")
        }
        // One byte at a time.
        var parser = SSEParser()
        XCTAssertEqual(bytes.compactMap { parser.push($0) }, expected)
    }

    func testMultiLineDataAndLineEndings() {
        let messages = parse(["data: first\r\ndata: second\rdata:third\n", "\r", "\n"])
        XCTAssertEqual(messages.map(\.data), ["first\nsecond\nthird"])
        XCTAssertEqual(messages.first?.event, "message")
    }

    func testCRLFSplitAcrossChunksIsOneLineEnd() {
        let messages = parse(["data: a\r", "\ndata: b\r", "\n\r", "\n"])
        XCTAssertEqual(messages.map(\.data), ["a\nb"])
    }

    func testIdIsKeptAcrossEventsWithoutId() {
        var parser = SSEParser()
        let messages = parse([
            "event: tool_called\nid: 5\ndata: {}\n\n",
            "event: agent_delta\ndata: {\"delta\":1}\n\n",
            "event: tool_returned\nid: 6\ndata: {}\n\n",
        ], parser: &parser)
        XCTAssertEqual(messages.map(\.lastEventId), ["5", "5", "6"])
        XCTAssertEqual(parser.lastEventId, "6")
    }

    func testCommentsFieldsWithoutValueAndEmptyEvents() {
        var parser = SSEParser(lastEventId: "3")
        let messages = parse([
            ": comment\n",
            "retry: 1000\nunknown: x\n\n",   // no data: not dispatched
            "id: 9\n\n",                       // id without data still counts
            "data\n\n",                        // field without colon: empty data
            "event: e\nid: a\0b\ndata:  two spaces\n\n", // id with NUL ignored; one space stripped
        ], parser: &parser)
        XCTAssertEqual(messages, [
            SSEMessage(event: "message", data: "", lastEventId: "9"),
            SSEMessage(event: "e", data: " two spaces", lastEventId: "9"),
        ])
    }

    func testFinishDropsAnIncompleteEvent() {
        var parser = SSEParser()
        XCTAssertEqual(parser.push(Array("data: partial\n".utf8)), [])
        parser.finish()
        XCTAssertEqual(parser.push(Array("data: next\n\n".utf8)).map(\.data), ["next"])
    }

    func testAnIdCountsOnlyWhenItsEventIsDispatched() {
        var parser = SSEParser(lastEventId: "3")
        XCTAssertEqual(parser.push(Array("data: a\nid: 7\n".utf8)), [])
        XCTAssertEqual(parser.lastEventId, "3")
        parser.finish()
        XCTAssertEqual(parser.lastEventId, "3")
        XCTAssertEqual(parser.push(Array("id: 8\ndata: b\n\n".utf8)).map(\.lastEventId), ["8"])
    }

    /// The global stream's `agent_delta` has no `id:`: `lastEventId` keeps
    /// the previous one, `id` says the message had none.
    func testAMessageWithoutIdKeepsTheLastIdButHasNoOwnId() {
        let messages = parse(["id: 5-1-2\ndata: a\n\nevent: agent_delta\ndata: b\n\n"])
        XCTAssertEqual(messages.map(\.lastEventId), ["5-1-2", "5-1-2"])
        XCTAssertEqual(messages.map(\.id), ["5-1-2", nil])
    }

    func testBackoffDoublesAndCaps() {
        let backoff = Backoff(initial: 0.5, maximum: 3)
        XCTAssertEqual((0..<5).map { backoff.delay(attempt: $0) }, [0.5, 1, 2, 3, 3])
    }
}

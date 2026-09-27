import Foundation

/// Decoders and encoders matching the server's JSON (chrono RFC 3339 dates,
/// snake case field names spelled out in `CodingKeys`).
public enum VibeJSON {
    /// A decoder for every payload of the API.
    public static func decoder() -> JSONDecoder {
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .custom { decoder in
            let container = try decoder.singleValueContainer()
            let text = try container.decode(String.self)
            guard let date = parseDate(text) else {
                throw DecodingError.dataCorruptedError(
                    in: container, debugDescription: "not an RFC 3339 date: \(text)")
            }
            return date
        }
        return decoder
    }

    /// An encoder for request bodies.
    public static func encoder() -> JSONEncoder {
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .custom { date, encoder in
            var container = encoder.singleValueContainer()
            try container.encode(formatDate(date))
        }
        return encoder
    }

    /// RFC 3339 in UTC with microseconds, as chrono writes it.
    public static func formatDate(_ date: Date) -> String {
        let seconds = date.timeIntervalSince1970.rounded(.down)
        let micros = Int(((date.timeIntervalSince1970 - seconds) * 1_000_000).rounded())
        let whole = wholeSeconds.string(from: Date(timeIntervalSince1970: seconds + (micros == 1_000_000 ? 1 : 0)))
        return whole.replacingOccurrences(of: "Z", with: String(format: ".%06dZ", micros % 1_000_000))
    }

    /// Parse RFC 3339 as chrono writes it: `Z` or an offset, with no
    /// fraction or with 1 to 9 fractional digits.
    public static func parseDate(_ text: String) -> Date? {
        var main = text
        var fraction = 0.0
        if let dot = text.firstIndex(of: ".") {
            let afterDot = text.index(after: dot)
            let digitsEnd = text[afterDot...].firstIndex(where: { !$0.isNumber }) ?? text.endIndex
            let digits = text[afterDot..<digitsEnd]
            if !digits.isEmpty, let value = Double("0." + digits) {
                fraction = value
            }
            main = String(text[..<dot]) + String(text[digitsEnd...])
        }
        guard let whole = wholeSeconds.date(from: main) else { return nil }
        return whole.addingTimeInterval(fraction)
    }

    private static let wholeSeconds: ISO8601DateFormatter = {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime]
        return formatter
    }()
}

extension KeyedDecodingContainer {
    /// `decodeIfPresent` with a default, for fields added after a log was written.
    func decode<T: Decodable>(_ type: T.Type, forKey key: Key, default value: T) throws -> T {
        try decodeIfPresent(type, forKey: key) ?? value
    }
}

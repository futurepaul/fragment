import Foundation

struct FragmentSummary: Codable, Identifiable, Hashable, Sendable {
    let name: String
    var kind: String
    var title: String?
    var agents: [String]?
    var preview: String?
    var previewAt: Int64?
    var archived: Bool?
    var id: String { name }
    var displayTitle: String { title ?? name }
}

struct SearchResults: Decodable, Sendable {
    var fragments: [FragmentSummary] = []
    var messages: [MessageHit] = []
}

struct MessageHit: Decodable, Identifiable, Sendable {
    let fragment: String
    let channel: String
    let seq: Int64
    let at: Int64
    let snippet: String
    var id: String { "\(fragment):\(channel):\(seq)" }
}

// Channels accept arbitrary JSON; unknown fields and record kinds must survive
// decoding without making an older app unable to read a newer chat.
enum JSONValue: Codable, Equatable, Sendable {
    case string(String), number(Double), bool(Bool), object([String: JSONValue]), array([JSONValue]), null

    init(from decoder: Decoder) throws {
        let value = try decoder.singleValueContainer()
        if value.decodeNil() { self = .null }
        else if let v = try? value.decode(String.self) { self = .string(v) }
        else if let v = try? value.decode(Bool.self) { self = .bool(v) }
        else if let v = try? value.decode(Double.self) { self = .number(v) }
        else if let v = try? value.decode([String: JSONValue].self) { self = .object(v) }
        else { self = .array(try value.decode([JSONValue].self)) }
    }

    func encode(to encoder: Encoder) throws {
        var value = encoder.singleValueContainer()
        switch self {
        case .string(let v): try value.encode(v)
        case .number(let v): try value.encode(v)
        case .bool(let v): try value.encode(v)
        case .object(let v): try value.encode(v)
        case .array(let v): try value.encode(v)
        case .null: try value.encodeNil()
        }
    }

    subscript(_ key: String) -> JSONValue {
        if case .object(let value) = self { return value[key] ?? .null }
        return .null
    }
    var string: String? { if case .string(let v) = self { return v }; return nil }
    var array: [JSONValue] { if case .array(let v) = self { return v }; return [] }
}

struct ChannelRecord: Codable, Identifiable, Equatable, Sendable {
    let channel: String
    let seq: Int64
    let at: Int64
    let principal: String
    let kind: String
    let body: JSONValue
    var id: String { "\(channel):\(seq)" }
    var text: String { body["text"].string ?? body.string ?? "" }
    var turn: String? { body["turn"].string }
    var isMessage: Bool {
        channel == "chat" && (body["kind"].string == nil || body["kind"].string == "message")
    }
}

struct LiveFrame: Decodable, Sendable {
    let type: String
    var channel: String?
    var seq: Int64?
    var at: Int64?
    var principal: String?
    var kind: String?
    var body: JSONValue?
    var turn: String?
    var text: String?
    var next: Int64?
    var more: Bool?
    var message: String?

    var record: ChannelRecord? {
        guard let channel, let seq, seq > 0, let at, let principal, let kind, let body else { return nil }
        return ChannelRecord(channel: channel, seq: seq, at: at, principal: principal, kind: kind, body: body)
    }
}

struct ReplyDraft: Identifiable, Equatable {
    let turn: String
    let principal: String
    var text: String
    var at: Int64
    var id: String { turn }
}

struct Timeline {
    private(set) var records: [ChannelRecord] = []
    private(set) var drafts: [String: ReplyDraft] = [:]
    private(set) var cursors: [String: Int64] = [:]
    // Bound the in-memory window independently of the server's retention.
    static let maximumRecords = 2_000

    mutating func insert(_ record: ChannelRecord, advancingCursor: Bool = true) {
        if advancingCursor { cursors[record.channel] = max(cursors[record.channel] ?? 0, record.seq) }
        if !records.contains(where: { $0.id == record.id }) {
            records.append(record)
            records.sort {
                if $0.at != $1.at { return $0.at < $1.at }
                if $0.channel != $1.channel { return $0.channel < $1.channel }
                return $0.seq < $1.seq
            }
            if records.count > Self.maximumRecords { records.removeFirst(records.count - Self.maximumRecords) }
        } else { return }
        if advancingCursor, record.channel == "chat", let turn = record.turn,
           drafts[turn]?.principal == record.principal {
            drafts.removeValue(forKey: turn)
        }
        if record.channel == "work", record.body["kind"].string == "turn.end", let turn = record.turn {
            drafts.removeValue(forKey: turn)
        }
    }

    mutating func apply(_ frame: LiveFrame) throws {
        switch frame.type {
        case "record":
            guard let record = frame.record else { throw APIError.invalidResponse }
            insert(record)
        case "draft":
            guard frame.channel == "chat", let turn = frame.turn, let principal = frame.principal else { return }
            if let text = frame.text {
                guard text.utf8.count <= 65_536, drafts.count < 32 || drafts[turn] != nil else {
                    throw APIError.limitExceeded
                }
                drafts[turn] = ReplyDraft(turn: turn, principal: principal, text: text, at: frame.at ?? 0)
            } else { drafts.removeValue(forKey: turn) }
        case "subscribed":
            if let channel = frame.channel, let next = frame.next {
                cursors[channel] = max(cursors[channel] ?? 0, next)
            }
        case "error": throw APIError.server(frame.message ?? "The live connection was refused.")
        default: break
        }
    }

    mutating func disconnected() { drafts.removeAll() }

    var activeTurns: [String] {
        var active: [String] = []
        for record in records where record.channel == "work" {
            guard let turn = record.turn else { continue }
            switch record.body["kind"].string {
            case "turn.start": if !active.contains(turn) { active.append(turn) }
            case "turn.end": active.removeAll { $0 == turn }
            default: break
            }
        }
        return active
    }
}

struct PendingPost: Identifiable {
    let id: String
    let text: String
    var failed = false
}

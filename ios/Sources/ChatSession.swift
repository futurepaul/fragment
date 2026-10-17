import Foundation
import Observation

@MainActor @Observable
final class ChatSession {
    let fragment: FragmentSummary
    let api: FragmentAPI
    private(set) var timeline = Timeline()
    private(set) var pending: [PendingPost] = []
    private(set) var connected = false
    private(set) var caughtUp = false
    private(set) var principal = ""
    private(set) var loadingHistory = false
    private(set) var hasEarlier = true
    var error: String?
    var composer = ""

    init(fragment: FragmentSummary, api: FragmentAPI) {
        self.fragment = fragment
        self.api = api
    }

    // Owned by the view's .task: backgrounding or leaving the chat closes the
    // socket, including a suspended receive. Each foreground visit catches up.
    func run() async {
        var attempt = 0
        while !Task.isCancelled {
            let socket = api.socket(fragment.name)
            socket.resume()
            do {
                try await withTaskCancellationHandler {
                    let heartbeat = Task {
                        while !Task.isCancelled {
                            try await Task.sleep(for: .seconds(20))
                            try await socket.send(.string("{\"type\":\"ping\"}"))
                        }
                    }
                    defer { heartbeat.cancel() }
                    while !Task.isCancelled {
                        let message = try await socket.receive()
                        let data: Data
                        switch message {
                        case .string(let text): data = Data(text.utf8)
                        case .data(let bytes): data = bytes
                        @unknown default: throw APIError.invalidResponse
                        }
                        let frame = try JSONDecoder().decode(LiveFrame.self, from: data)
                        if frame.type == "hello" {
                            principal = frame.principal ?? ""
                            connected = true
                            error = nil
                            attempt = 0
                            for channel in ["chat", "work"] {
                                try await subscribe(socket, channel: channel, after: timeline.cursors[channel])
                            }
                        }
                        try timeline.apply(frame)
                        if let clientID = frame.body?["client_id"].string, frame.principal == principal {
                            pending.removeAll { $0.id == clientID }
                        }
                        if frame.type == "subscribed", let channel = frame.channel {
                            if frame.more == true {
                                try await subscribe(socket, channel: channel, after: frame.next)
                            } else if channel == "chat" {
                                caughtUp = true
                                if timeline.records.filter({ $0.channel == "chat" }).count < 50 { hasEarlier = false }
                            }
                        }
                    }
                } onCancel: {
                    socket.cancel(with: .goingAway, reason: nil)
                }
            } catch {
                if !Task.isCancelled { self.error = "Connection interrupted. Reconnecting…" }
            }
            socket.cancel(with: .goingAway, reason: nil)
            connected = false
            caughtUp = false
            timeline.disconnected()
            guard !Task.isCancelled else { return }
            attempt = min(attempt + 1, 4)
            do { try await Task.sleep(for: .seconds(min(15, 1 << attempt))) }
            catch { return }
        }
    }

    private func subscribe(_ socket: URLSessionWebSocketTask, channel: String, after: Int64?) async throws {
        var body: [String: JSONValue] = ["type": .string("subscribe"), "channel": .string(channel)]
        if let after { body["after"] = .number(Double(after)) }
        else { body["last"] = .number(50) }
        let data = try JSONEncoder().encode(JSONValue.object(body))
        try await socket.send(.data(data))
    }

    func send() async {
        let text = composer.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty, text.utf8.count <= 32_768, pending.count < 16 else { return }
        let post = PendingPost(id: UUID().uuidString, text: text)
        pending.append(post)
        composer = ""
        await deliver(post)
    }

    func retry(_ post: PendingPost) async {
        guard let index = pending.firstIndex(where: { $0.id == post.id }), pending[index].failed else { return }
        pending[index].failed = false
        await deliver(post)
    }

    private func deliver(_ post: PendingPost) async {
        do {
            let record = try await api.post(fragment.name, id: post.id, body: .object([
                "text": .string(post.text), "client_id": .string(post.id)
            ]))
            timeline.insert(record, advancingCursor: false)
            pending.removeAll { $0.id == post.id }
            error = nil
        } catch {
            // A matching live record also acknowledges delivery. The HTTP
            // acknowledgement may have been lost after the append committed.
            guard let index = pending.firstIndex(where: { $0.id == post.id }) else { return }
            pending[index].failed = true
            self.error = error.localizedDescription
        }
    }

    func stop() async {
        guard let turn = timeline.activeTurns.first else { return }
        do {
            let record = try await api.post(fragment.name, id: "stop:\(turn)", body: .object([
                "kind": .string("stop"), "turn": .string(turn)
            ]))
            timeline.insert(record, advancingCursor: false)
        } catch { self.error = error.localizedDescription }
    }

    func loadEarlier() async {
        guard !loadingHistory, let first = timeline.records.filter({ $0.channel == "chat" }).map(\.seq).min() else {
            hasEarlier = false
            return
        }
        loadingHistory = true
        defer { loadingHistory = false }
        do {
            let page = try await api.history(fragment.name, before: first)
            for record in page { timeline.insert(record, advancingCursor: false) }
            hasEarlier = page.count == 50 && timeline.records.count < Timeline.maximumRecords
        } catch { self.error = error.localizedDescription }
    }
}

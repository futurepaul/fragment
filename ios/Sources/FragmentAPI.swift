import Foundation

enum APIError: LocalizedError {
    case invalidResponse, limitExceeded, server(String)
    var errorDescription: String? {
        switch self {
        case .invalidResponse: "Fragment sent an unreadable response."
        case .limitExceeded: "This response is larger than the app can display."
        case .server(let message): message
        }
    }
}

struct FragmentAPI: Sendable {
    let baseURL: URL
    let session: URLSession

    init(baseURL: URL, session: URLSession = .shared) {
        self.baseURL = baseURL
        self.session = session
    }

    func url(_ path: String) -> URL { baseURL.appending(path: path) }

    func request<T: Decodable>(_ path: String, method: String = "GET", body: JSONValue? = nil,
                              query: [URLQueryItem] = []) async throws -> T {
        var components = URLComponents(url: url(path), resolvingAgainstBaseURL: false)!
        components.queryItems = query.isEmpty ? nil : query
        var request = URLRequest(url: components.url!)
        request.httpMethod = method
        request.timeoutInterval = 30
        if let body {
            request.httpBody = try JSONEncoder().encode(body)
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }
        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse else { throw APIError.invalidResponse }
        guard (200..<300).contains(http.statusCode) else {
            let problem = try? JSONDecoder().decode(JSONValue.self, from: data)
            throw APIError.server(problem?["message"].string ?? "Fragment returned HTTP \(http.statusCode).")
        }
        guard data.count <= 4_194_304 else { throw APIError.limitExceeded }
        return try JSONDecoder().decode(T.self, from: data)
    }

    func fragments() async throws -> [FragmentSummary] {
        struct Response: Decodable { let fragments: [FragmentSummary] }
        let response: Response = try await request("api/fragments")
        return response.fragments
    }

    // Search is only a hint: the complete prompt, including words beyond the
    // API's eight-word / 256-byte query limit, remains the new chat's message.
    static func searchHint(_ text: String) -> String {
        let words = text.split(whereSeparator: { $0.isWhitespace || $0.isNewline }).prefix(8)
        var hint = ""
        for character in words.joined(separator: " ") {
            guard hint.utf8.count + String(character).utf8.count <= 256 else { break }
            hint.append(character)
        }
        return hint
    }

    func search(_ text: String) async throws -> SearchResults {
        let hint = Self.searchHint(text)
        guard !hint.isEmpty else { return SearchResults() }
        return try await request("api/search", query: [URLQueryItem(name: "q", value: hint)])
    }

    func createChat() async throws -> FragmentSummary {
        struct Created: Decodable { let name: String }
        let created: Created = try await request("api/fragments", method: "POST", body: .object([
            "label": .string("mobile-chat"), "template": .string("chat"), "title": .string("New chat")
        ]))
        return FragmentSummary(name: created.name, kind: "chat", title: "New chat")
    }

    func join(_ agent: FragmentSummary, to chat: FragmentSummary) async throws {
        struct Status: Decodable { let npub: String }
        let status: Status = try await request("api/f/\(agent.name)/status")
        let _: JSONValue = try await request("api/f/\(chat.name)/members/\(status.npub)",
            method: "PUT", body: .object(["role": .string("editor")]))
    }

    func post(_ fragment: String, id: String, body: JSONValue) async throws -> ChannelRecord {
        struct Posted: Decodable { let record: ChannelRecord }
        let result: Posted = try await request("api/f/\(fragment)/channels/chat", method: "POST",
            body: .object(["id": .string(id), "body": body]))
        return result.record
    }

    func history(_ fragment: String, before: Int64) async throws -> [ChannelRecord] {
        struct Page: Decodable { let records: [ChannelRecord] }
        var components = URLComponents(url: url("api/f/\(fragment)/channels/chat"), resolvingAgainstBaseURL: false)!
        components.queryItems = [URLQueryItem(name: "before", value: String(before)), URLQueryItem(name: "limit", value: "50")]
        let (data, response) = try await session.data(from: components.url!)
        guard (response as? HTTPURLResponse)?.statusCode == 200 else { throw APIError.server("Earlier messages could not be loaded.") }
        guard data.count <= 4_194_304 else { throw APIError.limitExceeded }
        return try JSONDecoder().decode(Page.self, from: data).records
    }

    func socket(_ fragment: String) -> URLSessionWebSocketTask {
        var components = URLComponents(url: url("f/\(fragment)/__live"), resolvingAgainstBaseURL: false)!
        components.scheme = baseURL.scheme == "https" ? "wss" : "ws"
        let socket = session.webSocketTask(with: components.url!)
        socket.maximumMessageSize = 1_048_576
        return socket
    }
}

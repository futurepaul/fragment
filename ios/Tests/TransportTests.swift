import Foundation
import Testing
@testable import Fragment

@MainActor
struct TransportTests {
    private let api = FragmentAPI(baseURL: URL(string: "http://localhost:8910")!)

    private func newChat() async throws -> FragmentSummary {
        let fragments = try await api.fragments()
        let chat = try await api.createChat()
        try await api.join(#require(fragments.first { $0.kind == "agent" }), to: chat)
        return chat
    }

    private func waitUntil(_ condition: @MainActor () -> Bool, timeout: TimeInterval = 12) async throws {
        let deadline = Date().addingTimeInterval(timeout)
        while !condition(), Date() < deadline { try await Task.sleep(for: .milliseconds(30)) }
        #expect(condition(), "Timed out waiting for the real HTTP/WebSocket fixture")
    }

    @Test func streamStopAndReconnect() async throws {
        let chat = try await newChat()
        let session = ChatSession(fragment: chat, api: api)
        let stream = Task { await session.run() }
        defer { stream.cancel() }
        try await waitUntil { session.connected && session.caughtUp }
        session.composer = "A slow reply please"
        await session.send()
        try await waitUntil { !session.timeline.drafts.isEmpty }
        #expect(session.timeline.records.filter(\.isMessage).count == 1)
        await session.stop()
        try await waitUntil { session.timeline.activeTurns.isEmpty }
        #expect(session.timeline.drafts.isEmpty)
        session.composer = "Test /disconnect during the next reply"
        await session.send()
        try await waitUntil({ session.timeline.records.filter { $0.isMessage && $0.principal != session.principal }.count == 1 }, timeout: 18)
        #expect(session.timeline.records.filter { $0.isMessage && $0.principal == session.principal }.count == 2)
        #expect(session.timeline.drafts.isEmpty)
        stream.cancel()
        await stream.value
        #expect(!session.connected)
    }

    @Test func lostAcknowledgementRetryIsSameRecordAndSameTurn() async throws {
        let chat = try await newChat()
        let id = UUID().uuidString
        let body = JSONValue.object(["text": .string("Test /fail-once"), "client_id": .string(id)])
        do {
            _ = try await api.post(chat.name, id: id, body: body)
            Issue.record("The fixture must lose the first acknowledgement")
        } catch { /* The append committed; its HTTP acknowledgement was lost. */ }
        let first = try await api.post(chat.name, id: id, body: body)
        let second = try await api.post(chat.name, id: id, body: body)
        #expect(first == second)
        let records = try await api.history(chat.name, before: Int64.max)
        #expect(records.filter { $0.principal == "npub1localperson" }.count == 1)
        do {
            _ = try await api.post(chat.name, id: id, body: .object(["text": .string("changed")]))
            Issue.record("Reusing a post id with a changed body must fail")
        } catch { }
    }

    @Test func earlierHistoryIsExclusiveAndAscending() async throws {
        let records = try await api.history("long-chat", before: 31)
        #expect(records.count == 30)
        #expect(records.first?.seq == 1)
        #expect(records.last?.seq == 30)
    }
}

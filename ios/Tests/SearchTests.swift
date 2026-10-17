import Foundation
import Testing
@testable import Fragment

struct SearchTests {
    @Test func longPromptOnlyBoundsSearchHint() {
        let prompt = "one two three four five six seven eight nine ten"
        #expect(FragmentAPI.searchHint(prompt) == "one two three four five six seven eight")
        let unicode = String(repeating: "🙂", count: 100)
        let hint = FragmentAPI.searchHint(unicode)
        #expect(hint.utf8.count == 256)
        #expect(FragmentAPI.searchHint(" \n\t ").isEmpty)
        #expect(FragmentAPI.searchHint("one\ntwo") == "one two")
    }

    @Test func serverSearchFindsMessageContentsAndAppNames() async throws {
        let api = FragmentAPI(baseURL: URL(string: "http://localhost:8910")!)
        let messages = try await api.search("quiet minutes")
        #expect(messages.fragments.isEmpty)
        #expect(messages.messages.contains { $0.fragment == "morning" && $0.seq == 2 && $0.snippet.contains("quiet minutes") })
        let apps = try await api.search("Reading")
        #expect(apps.fragments.contains { $0.name == "reading" && $0.kind == "app" })
        let empty = try await api.search(" ")
        #expect(empty.fragments.isEmpty && empty.messages.isEmpty)
        let absent = try await api.search("no-such-fragment-or-message-8675309")
        #expect(absent.fragments.isEmpty && absent.messages.isEmpty)
    }
}

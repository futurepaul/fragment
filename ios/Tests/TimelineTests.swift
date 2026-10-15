import Foundation
import Testing
@testable import Fragment

struct TimelineTests {
    private func record(_ seq: Int64, channel: String = "chat", principal: String = "agent", body: JSONValue) -> ChannelRecord {
        ChannelRecord(channel: channel, seq: seq, at: 100, principal: principal, kind: "message", body: body)
    }
    private func frame(_ json: String) throws -> LiveFrame { try JSONDecoder().decode(LiveFrame.self, from: Data(json.utf8)) }

    @Test func replayAndOlderPagesNeverRegressLiveCursor() {
        var timeline = Timeline()
        let latest = record(20, body: .string("latest"))
        timeline.insert(latest)
        timeline.insert(latest)
        timeline.insert(record(2, body: .string("earlier")), advancingCursor: false)
        timeline.insert(record(10, body: .string("middle")), advancingCursor: false)
        #expect(timeline.records.map(\.seq) == [2, 10, 20])
        #expect(timeline.cursors["chat"] == 20)
    }

    @Test func draftIsWholeTextAndReplacedOnlyByItsAuthorsCommit() throws {
        var timeline = Timeline()
        try timeline.apply(frame(#"{"type":"draft","channel":"chat","turn":"a","principal":"agent","text":"One","at":100}"#))
        try timeline.apply(frame(#"{"type":"draft","channel":"chat","turn":"a","principal":"agent","text":"One two","at":101}"#))
        #expect(timeline.drafts["a"]?.text == "One two")
        timeline.insert(record(1, principal: "someone-else", body: .object(["turn": .string("a"), "text": .string("forged")])))
        #expect(timeline.drafts["a"] != nil)
        timeline.insert(record(2, body: .object(["turn": .string("a"), "text": .string("One two")])))
        #expect(timeline.drafts.isEmpty)
        #expect(timeline.records.count == 2)
    }

    @Test func replayedRecordDoesNotEraseNextPartOfTurn() throws {
        var timeline = Timeline()
        let commit = record(1, body: .object(["turn": .string("a"), "text": .string("First part")]))
        timeline.insert(commit)
        try timeline.apply(frame(#"{"type":"draft","channel":"chat","turn":"a","principal":"agent","text":"Next part","at":110}"#))
        timeline.insert(commit)
        #expect(timeline.drafts["a"]?.text == "Next part")
        timeline.disconnected()
        #expect(timeline.drafts.isEmpty)
        #expect(timeline.records.count == 1)
    }

    @Test func channelsHaveIndependentCursorsAndUnknownKindsDecode() throws {
        var timeline = Timeline()
        try timeline.apply(frame(#"{"type":"record","channel":"chat","seq":8,"at":1,"principal":"person","kind":"message","body":"plain text"}"#))
        try timeline.apply(frame(#"{"type":"record","channel":"work","seq":2,"at":2,"principal":"agent","kind":"message","body":{"kind":"future.kind","newField":true}}"#))
        #expect(timeline.cursors == ["chat": 8, "work": 2])
        #expect(timeline.records.first?.text == "plain text")
        #expect(timeline.records.last?.isMessage == false)
        #expect(throws: (any Error).self) {
            try timeline.apply(frame(#"{"type":"record","channel":"chat","seq":-1}"#))
        }
    }

    @Test func turnEndClearsWorkingAndDraft() throws {
        var timeline = Timeline()
        timeline.insert(record(1, channel: "work", body: .object(["kind": .string("turn.start"), "turn": .string("a")])))
        #expect(timeline.activeTurns == ["a"])
        try timeline.apply(frame(#"{"type":"draft","channel":"chat","turn":"a","principal":"agent","text":"unfinished","at":100}"#))
        timeline.insert(record(2, channel: "work", body: .object(["kind": .string("turn.end"), "turn": .string("a")])))
        #expect(timeline.activeTurns.isEmpty)
        #expect(timeline.drafts.isEmpty)
    }
}

import Testing
@testable import Fragment

struct MarkdownTests {
    @Test func partialCodeFenceStaysCodeDuringStreaming() {
        let partial = MarkdownBlock.parse("Hello\n\n```swift\nlet value = 1")
        #expect(partial.count == 2)
        #expect(partial.last?.kind == .code("swift"))
        let completed = MarkdownBlock.parse("Hello\n\n```swift\nlet value = 1\n```\n\nNext")
        #expect(completed[1] == partial[1])
        #expect(completed.last?.text == "Next")
    }
    @Test func headingListQuoteAndInlineMarkdownStayNative() {
        let blocks = MarkdownBlock.parse("## Title\n\n- First\n- **Second**\n\n> A thought")
        #expect(blocks.map(\.kind) == [.heading, .paragraph, .quote])
        #expect(blocks[1].text == "• First\n• **Second**")
    }
}

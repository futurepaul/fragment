import SwiftUI

// A small native renderer for the chat slice: paragraphs, headings, lists,
// quotes and fenced code. Inline formatting and links are Apple's Markdown.
// Blocks have positional identities so a growing final block stays in place.
struct MarkdownBlock: Identifiable, Equatable {
    enum Kind: Equatable { case paragraph, heading, code(String), quote }
    let id: Int
    let kind: Kind
    let text: String

    static func parse(_ source: String) -> [MarkdownBlock] {
        var blocks: [MarkdownBlock] = []
        var lines: [String] = []
        var code: String?
        func append(_ kind: Kind) {
            guard !lines.isEmpty else { return }
            blocks.append(MarkdownBlock(id: blocks.count, kind: kind, text: lines.joined(separator: "\n")))
            lines = []
        }
        for line in source.components(separatedBy: "\n") {
            if line.hasPrefix("```") {
                if let language = code { append(.code(language)); code = nil }
                else { append(.paragraph); code = String(line.dropFirst(3)) }
            } else if code != nil { lines.append(line) }
            else if line.trimmingCharacters(in: .whitespaces).isEmpty { append(.paragraph) }
            else if line.hasPrefix("#") {
                let heading = line.drop(while: { $0 == "#" })
                if heading.first == " " {
                    append(.paragraph)
                    lines = [String(heading.dropFirst())]
                    append(.heading)
                } else { lines.append(line) }
            } else if line.hasPrefix("> ") {
                append(.paragraph)
                lines = [String(line.dropFirst(2))]
                append(.quote)
            } else {
                lines.append(line.hasPrefix("- ") || line.hasPrefix("* ") ? "• " + line.dropFirst(2) : line)
            }
        }
        append(code.map(Kind.code) ?? .paragraph)
        return blocks
    }
}

struct MarkdownText: View {
    let text: String
    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            ForEach(MarkdownBlock.parse(text)) { block in
                switch block.kind {
                case .code(let language):
                    VStack(alignment: .leading, spacing: 10) {
                        HStack {
                            Text(language.isEmpty ? "Code" : language).font(.caption).foregroundStyle(.secondary)
                            Spacer()
                            Button("Copy code", systemImage: "doc.on.doc") { UIPasteboard.general.string = block.text }
                                .labelStyle(.iconOnly).font(.caption).frame(minWidth: 44, minHeight: 32)
                        }
                        ScrollView(.horizontal) { Text(block.text).font(.callout.monospaced()).textSelection(.enabled) }
                    }
                    .padding(14).background(FragmentStyle.surface, in: RoundedRectangle(cornerRadius: 14))
                case .heading:
                    inline(block.text).font(.title3.weight(.semibold))
                case .quote:
                    HStack(alignment: .top, spacing: 12) {
                        RoundedRectangle(cornerRadius: 2).fill(.secondary.opacity(0.3)).frame(width: 3)
                        inline(block.text).foregroundStyle(.secondary)
                    }.fixedSize(horizontal: false, vertical: true)
                case .paragraph:
                    inline(block.text).font(.body).lineSpacing(5)
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
    private func inline(_ source: String) -> some View {
        Text((try? AttributedString(markdown: source, options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace))) ?? AttributedString(source))
            .textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
    }
}

import SwiftUI

struct ChatView: View {
    @Environment(\.scenePhase) private var scenePhase
    @State private var session: ChatSession
    @State private var followsBottom = true
    @State private var scrollingByHand = false
    @State private var initialText: String?
    let openLibrary: () -> Void
    let consumeInitialText: () -> Void

    init(fragment: FragmentSummary, api: FragmentAPI, initialText: String?, consumeInitialText: @escaping () -> Void,
         openLibrary: @escaping () -> Void) {
        _session = State(initialValue: ChatSession(fragment: fragment, api: api))
        _initialText = State(initialValue: initialText)
        self.openLibrary = openLibrary
        self.consumeInitialText = consumeInitialText
    }

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 24) {
                    if session.hasEarlier && !session.timeline.records.isEmpty {
                        Button("Earlier messages") { Task { await session.loadEarlier() } }
                            .font(.footnote).frame(maxWidth: .infinity).disabled(session.loadingHistory)
                    }
                    ForEach(session.timeline.records) { record in
                        if record.isMessage {
                            MessageView(text: record.text, isYou: record.principal == session.principal)
                                .id(record.id)
                        } else if record.channel == "work" {
                            WorkRecordView(record: record)
                        }
                    }
                    ForEach(session.pending) { pending in
                        VStack(alignment: .trailing, spacing: 6) {
                            MessageView(text: pending.text, isYou: true)
                            if pending.failed {
                                Button("Not sent. Tap to retry", systemImage: "arrow.clockwise") {
                                    Task { await session.retry(pending) }
                                }.font(.caption).accessibilityIdentifier("retry")
                            } else { Text("Sending…").font(.caption).foregroundStyle(.secondary) }
                        }.frame(maxWidth: .infinity, alignment: .trailing)
                    }
                    ForEach(session.timeline.drafts.values.sorted { $0.at < $1.at }) { draft in
                        MessageView(text: draft.text, isYou: false, streaming: true)
                    }
                    if !session.timeline.activeTurns.isEmpty && session.timeline.drafts.isEmpty {
                        Label("Working…", systemImage: "ellipsis").font(.callout).foregroundStyle(.secondary)
                    }
                    Color.clear.frame(height: 1).id("bottom")
                }
                .frame(maxWidth: 660, alignment: .leading)
                .padding(.horizontal, 20).padding(.vertical, 24).frame(maxWidth: .infinity)
            }
            .scrollDismissesKeyboard(.interactively)
            .accessibilityIdentifier("timeline")
            .defaultScrollAnchor(.bottom, for: .initialOffset)
            .defaultScrollAnchor(.top, for: .alignment)
            .onScrollGeometryChange(for: Bool.self) { geometry in
                geometry.contentSize.height - geometry.visibleRect.maxY < 100
            } action: { _, nearBottom in
                if scrollingByHand { followsBottom = nearBottom }
            }
            .onScrollPhaseChange { _, phase in
                scrollingByHand = phase == .interacting || phase == .decelerating
            }
            .onChange(of: session.timeline.records.count + session.pending.count) { _, _ in
                if followsBottom { proxy.scrollTo("bottom", anchor: .bottom) }
            }
            .onChange(of: session.timeline.drafts) { _, _ in
                if followsBottom { proxy.scrollTo("bottom", anchor: .bottom) }
            }
            .overlay(alignment: .bottomTrailing) {
                if !followsBottom {
                    Button("Latest messages", systemImage: "arrow.down") {
                        followsBottom = true
                        withAnimation { proxy.scrollTo("bottom", anchor: .bottom) }
                    }
                    .labelStyle(.iconOnly).buttonStyle(.borderedProminent).buttonBorderShape(.circle).padding()
                }
            }
            .overlay {
                if session.timeline.records.isEmpty && session.pending.isEmpty {
                    ContentUnavailableView("A fresh start", systemImage: "bubble.left.and.bubble.right",
                        description: Text("A new conversation with your agent."))
                }
            }
        }
        .background(FragmentStyle.background)
        .safeAreaInset(edge: .bottom, spacing: 0) {
            VStack(spacing: 0) {
                if let error = session.error {
                    Text(error).font(.caption).foregroundStyle(.secondary).padding(.horizontal, 20).padding(.top, 8)
                }
                ComposerView(text: $session.composer, working: !session.timeline.activeTurns.isEmpty,
                    enabled: session.connected && session.caughtUp, send: { await session.send() }, stop: { await session.stop() })
            }.background(FragmentStyle.background)
        }
        .navigationTitle(session.fragment.displayTitle)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) { Button("Chats", systemImage: "line.3.horizontal", action: openLibrary) }
        }
        .task(id: scenePhase) {
            guard scenePhase == .active else { return }
            await session.run()
        }
        .task {
            if let text = initialText {
                initialText = nil
                consumeInitialText()
                session.composer = text
                await session.send()
            }
        }
    }
}

struct MessageView: View {
    let text: String
    let isYou: Bool
    var streaming = false

    var body: some View {
        HStack(alignment: .top, spacing: 0) {
            if isYou { Spacer(minLength: 44) }
            VStack(alignment: .leading, spacing: 8) {
                if !isYou {
                    HStack(spacing: 7) {
                        Circle().fill(FragmentStyle.lilac).frame(width: 8, height: 8)
                        Text("Fragment").font(.caption.weight(.medium)).foregroundStyle(.secondary)
                        if streaming { ProgressView().controlSize(.mini).accessibilityLabel("Replying") }
                    }
                }
                if isYou {
                    Text(text).font(.body).lineSpacing(5).textSelection(.enabled)
                        .fixedSize(horizontal: false, vertical: true)
                } else { MarkdownText(text: text) }
            }
            .padding(isYou ? 14 : 0)
            .background(isYou ? FragmentStyle.bubble : .clear, in: RoundedRectangle(cornerRadius: 20))
            if !isYou { Spacer(minLength: 8) }
        }
        .frame(maxWidth: .infinity, alignment: isYou ? .trailing : .leading)
        .contextMenu { Button("Copy", systemImage: "doc.on.doc") { UIPasteboard.general.string = text } }
        .accessibilityElement(children: .combine)
    }
}

struct WorkRecordView: View {
    let record: ChannelRecord
    var body: some View {
        if record.body["kind"].string == "turn.step" {
            DisclosureGroup {
                Text(record.body["args"].string ?? "").font(.caption.monospaced()).textSelection(.enabled)
                Text(record.body["excerpt"].string ?? "").font(.caption).textSelection(.enabled)
            } label: {
                Label(record.body["tool"].string ?? "Working", systemImage: "checkmark.circle")
                    .font(.callout).foregroundStyle(.secondary)
            }
            .padding(14).background(FragmentStyle.surface, in: RoundedRectangle(cornerRadius: 14))
        } else if record.body["kind"].string == "turn.end", let error = record.body["error"].string {
            Label(error, systemImage: "exclamationmark.circle").font(.callout).foregroundStyle(.secondary)
        } else if record.body["kind"].string == "turn.end", record.body["status"].string == "stopped" {
            Text("Response stopped").font(.footnote).foregroundStyle(.secondary)
        } else if record.body["kind"].string == "turn.prompt" {
            Label("\(record.text)\nOpen this chat on the web to answer this request.", systemImage: "questionmark.bubble")
                .font(.callout).foregroundStyle(.secondary)
        }
    }
}

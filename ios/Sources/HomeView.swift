import SwiftUI

struct HomeView: View {
    let library: Library
    @Binding var draft: String
    let open: (FragmentSummary) -> Void
    let start: () async -> Void
    let refresh: () async -> Void

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 36) {
                VStack(alignment: .leading, spacing: 20) {
                    Circle().fill(FragmentStyle.lilac.gradient)
                        .frame(width: 46, height: 46)
                        .accessibilityHidden(true)
                    VStack(alignment: .leading, spacing: 10) {
                        Text("A little space\nto think big.")
                            .font(.system(.largeTitle, design: .serif).weight(.medium))
                            .fixedSize(horizontal: false, vertical: true)
                        Text("What would you like to work on?")
                            .font(.body).foregroundStyle(.secondary)
                    }
                }
                .padding(.top, 42)
                if !library.chats.isEmpty {
                    VStack(alignment: .leading, spacing: 12) {
                        Text("PICK UP WHERE YOU LEFT OFF")
                            .font(.caption2.weight(.medium)).tracking(1.5).foregroundStyle(.secondary)
                        VStack(spacing: 0) {
                            ForEach(library.chats.prefix(3)) { chat in
                                Button { open(chat) } label: { ChatSummaryRow(chat: chat) }
                                    .buttonStyle(.plain)
                                if chat.id != library.chats.prefix(3).last?.id { Divider().padding(.leading, 16) }
                            }
                        }
                        .background(FragmentStyle.surface, in: RoundedRectangle(cornerRadius: 20))
                    }
                } else if library.loading { ProgressView("Connecting to Fragment…") }
                if let error = library.error {
                    Label(error, systemImage: "wifi.exclamationmark").font(.callout).foregroundStyle(.secondary)
                    Button("Try again") { Task { await refresh() } }
                }
            }
            .frame(maxWidth: 620, alignment: .leading)
            .padding(.horizontal, 24).padding(.bottom, 32)
            .frame(maxWidth: .infinity)
        }
        .background(FragmentStyle.background)
        .refreshable { await refresh() }
        .safeAreaInset(edge: .bottom, spacing: 0) {
            ComposerView(text: $draft, working: false, enabled: library.agent != nil && !library.creating,
                sending: library.creating, send: start, stop: {})
        }
        .navigationTitle("Fragment")
        .navigationBarTitleDisplayMode(.inline)
    }
}

struct ChatSummaryRow: View {
    let chat: FragmentSummary
    var body: some View {
        HStack(spacing: 14) {
            VStack(alignment: .leading, spacing: 5) {
                Text(chat.displayTitle).font(.body.weight(.medium)).lineLimit(1)
                Text(chat.preview ?? "Start a conversation").font(.subheadline).foregroundStyle(.secondary).lineLimit(1)
            }
            Spacer(minLength: 8)
            Image(systemName: "chevron.right").font(.caption.weight(.semibold)).foregroundStyle(.tertiary)
        }
        .padding(16).contentShape(Rectangle())
        .accessibilityElement(children: .combine)
    }
}

struct ComposerView: View {
    @Binding var text: String
    let working: Bool
    let enabled: Bool
    var sending = false
    let send: () async -> Void
    let stop: () async -> Void
    @FocusState private var focused: Bool
    @State private var submitting = false

    private var valid: Bool { !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && text.utf8.count <= 32_768 }

    var body: some View {
        HStack(alignment: .bottom, spacing: 8) {
            TextField("Message your agent", text: $text, axis: .vertical)
                .lineLimit(1...6).padding(.vertical, 12).padding(.leading, 18)
                .focused($focused).accessibilityIdentifier("composer")
            Button {
                submitting = true
                Task { await (working ? stop() : send()); submitting = false }
            } label: {
                Group {
                    if sending || submitting { ProgressView().tint(FragmentStyle.background) }
                    else { Image(systemName: working ? "stop.fill" : "arrow.up").font(.body.weight(.semibold)) }
                }
                .frame(width: 38, height: 38)
                .foregroundStyle(FragmentStyle.background)
                .background(.primary.opacity((working || valid) && enabled ? 1 : 0.25), in: Circle())
                .frame(width: 46, height: 46)
            }
            .disabled(submitting || sending || !enabled || (!working && !valid))
            .accessibilityLabel(working ? "Stop response" : "Send message")
            .accessibilityIdentifier(working ? "stop" : "send")
            .padding(.bottom, 2).padding(.trailing, 2)
        }
        .background(FragmentStyle.surface, in: RoundedRectangle(cornerRadius: 27))
        .overlay { RoundedRectangle(cornerRadius: 27).strokeBorder(.primary.opacity(0.08)) }
        .frame(maxWidth: 660)
        .padding(.horizontal, 16).padding(.top, 10).padding(.bottom, 8)
        .frame(maxWidth: .infinity)
        .background(FragmentStyle.background)
    }
}

struct LibraryView: View {
    @Environment(\.dismiss) private var dismiss
    let library: Library
    let api: FragmentAPI
    let open: (FragmentSummary) -> Void
    let newChat: () -> Void
    @State private var search = ""

    private func matches(_ fragment: FragmentSummary) -> Bool {
        search.isEmpty || fragment.displayTitle.localizedCaseInsensitiveContains(search)
            || (fragment.preview ?? "").localizedCaseInsensitiveContains(search)
    }

    var body: some View {
        NavigationStack {
            List {
                Button("New chat", systemImage: "square.and.pencil", action: newChat)
                if !library.apps.isEmpty {
                    Section("Apps") {
                        ForEach(library.apps.filter(matches)) { app in
                            Button { open(app) } label: { Label(app.displayTitle, systemImage: "square.grid.2x2") }
                        }
                    }
                }
                Section("Chats") {
                    ForEach(library.chats.filter(matches)) { chat in
                        Button { open(chat) } label: {
                            VStack(alignment: .leading, spacing: 5) {
                                Text(chat.displayTitle).foregroundStyle(.primary)
                                Text(chat.preview ?? "No messages yet").font(.subheadline).foregroundStyle(.secondary).lineLimit(1)
                            }.padding(.vertical, 5)
                        }
                    }
                }
            }
            .searchable(text: $search, prompt: "Find a chat or app")
            .refreshable { await library.load(api) }
            .navigationTitle("Your Fragment")
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }
            .task { await library.load(api) }
        }
        .presentationDragIndicator(.visible)
    }
}

#Preview("Home") {
    NavigationStack { HomeView(library: Library(), draft: .constant(""), open: { _ in }, start: {}, refresh: {}) }
}

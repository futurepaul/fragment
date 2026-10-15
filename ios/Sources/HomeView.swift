import SwiftUI

struct HomeView: View {
    let library: Library
    let api: FragmentAPI
    @Binding var draft: String
    let open: (FragmentSummary) -> Void
    let start: () async -> Void
    let refresh: () async -> Void
    @State private var results = SearchResults()
    @State private var searching = false
    @State private var searchError: String?
    @FocusState private var focused: Bool

    private var query: String { draft.trimmingCharacters(in: .whitespacesAndNewlines) }
    private var canStart: Bool {
        !query.isEmpty && query.utf8.count <= 32_768 && library.agent != nil && !library.creating
    }

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 12) {
                Image(systemName: "magnifyingglass").foregroundStyle(.secondary)
                TextField("Ask anything or search", text: $draft)
                    .focused($focused).submitLabel(.go)
                    .accessibilityIdentifier("launcherInput")
                    .disabled(library.creating)
                    .onSubmit(launch)
                if !draft.isEmpty {
                    Button("Clear search", systemImage: "xmark.circle.fill") { draft = "" }
                        .labelStyle(.iconOnly).foregroundStyle(.secondary)
                        .disabled(library.creating)
                }
                Button(action: launch) {
                    Group {
                        if library.creating { ProgressView() }
                        else { Image(systemName: "arrow.turn.down.left") }
                    }.frame(width: 44, height: 44)
                }
                .disabled(!canStart).accessibilityLabel("Start new chat")
                .accessibilityIdentifier("startChat")
            }
            .padding(.leading, 16).padding(.trailing, 4)
            .background(FragmentStyle.surface, in: RoundedRectangle(cornerRadius: 18))
            .overlay { RoundedRectangle(cornerRadius: 18).strokeBorder(.primary.opacity(0.08)) }
            .padding(.horizontal, 16).padding(.top, 12)

            if query.isEmpty {
                VStack(spacing: 8) {
                    Text("Search chats, apps, and messages.")
                    Text("Or start a new chat with \(library.agent?.displayTitle ?? "your agent").")
                }
                .font(.subheadline).foregroundStyle(.secondary)
                .multilineTextAlignment(.center).padding(32)
                Spacer()
            } else {
                List {
                    if library.agent != nil {
                        Button(action: launch) {
                            HStack(spacing: 14) {
                                Image(systemName: "square.and.pencil").foregroundStyle(FragmentStyle.lilac)
                                VStack(alignment: .leading, spacing: 5) {
                                    Text("Start a new chat with \(library.agent?.displayTitle ?? "your agent")")
                                        .font(.body.weight(.medium))
                                    Text(query).font(.subheadline).foregroundStyle(.secondary).lineLimit(2)
                                }
                                Spacer(minLength: 0)
                            }.padding(.vertical, 6)
                        }
                        .disabled(!canStart).accessibilityIdentifier("newChatResult")
                    }
                    foundFragments("Chats", kinds: ["chat"])
                    foundFragments("Apps", kinds: ["app", "brain"])
                    if !results.messages.isEmpty {
                        Section("Messages") {
                            ForEach(results.messages) { hit in
                                if let fragment = library.fragments.first(where: { $0.name == hit.fragment }) {
                                    Button { focused = false; open(fragment) } label: {
                                        VStack(alignment: .leading, spacing: 5) {
                                            Text(fragment.displayTitle).font(.body.weight(.medium))
                                            Text(hit.snippet).font(.subheadline).foregroundStyle(.secondary).lineLimit(3)
                                        }.padding(.vertical, 6)
                                    }.accessibilityIdentifier("messageResult-\(hit.id)")
                                }
                            }
                        }
                    }
                    if searching { ProgressView("Searching…").listRowBackground(Color.clear) }
                    if let searchError { Text(searchError).font(.footnote).foregroundStyle(.secondary) }
                }
                .scrollContentBackground(.hidden).scrollDismissesKeyboard(.interactively)
            }
            if let error = library.error {
                VStack(spacing: 8) {
                    Text(error).font(.footnote).foregroundStyle(.secondary)
                    Button("Try again") { Task { await refresh() } }
                }.padding()
            }
        }
        .frame(maxWidth: 700).frame(maxWidth: .infinity)
        .background(FragmentStyle.background)
        .navigationTitle("Fragment").navigationBarTitleDisplayMode(.inline)
        .background {
            Button("Search everything") { focused = true }
                .keyboardShortcut("k", modifiers: .command).hidden().accessibilityHidden(true)
        }
        .task(id: query) {
            results = SearchResults()
            searchError = nil
            searching = false
            guard !query.isEmpty else { return }
            let asked = query
            do {
                try await Task.sleep(for: .milliseconds(250))
                searching = true
                let found = try await api.search(asked)
                try Task.checkCancellation()
                guard asked == query else { return }
                results = found
                searching = false
            } catch {
                guard !Task.isCancelled, asked == query else { return }
                searchError = error.localizedDescription
                searching = false
            }
        }
    }

    @ViewBuilder private func foundFragments(_ heading: String, kinds: Set<String>) -> some View {
        let found = results.fragments.filter { kinds.contains($0.kind) }
        if !found.isEmpty {
            Section(heading) {
                ForEach(found) { fragment in
                    Button { focused = false; open(fragment) } label: {
                        HStack(spacing: 12) {
                            Image(systemName: fragment.kind == "chat" ? "bubble.left" : "square.grid.2x2")
                                .foregroundStyle(.secondary)
                            VStack(alignment: .leading, spacing: 5) {
                                Text(fragment.displayTitle).foregroundStyle(.primary)
                                if let preview = fragment.preview {
                                    Text(preview).font(.subheadline).foregroundStyle(.secondary).lineLimit(1)
                                }
                            }
                            if fragment.archived == true { Text("Archived").font(.caption).foregroundStyle(.secondary) }
                        }.padding(.vertical, 6)
                    }.accessibilityIdentifier("fragmentResult-\(fragment.name)")
                }
            }
        }
    }

    private func launch() {
        guard canStart else { return }
        focused = false
        Task { await start() }
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
    NavigationStack { HomeView(library: Library(), api: FragmentAPI(baseURL: URL(string: "http://localhost:8910")!), draft: .constant(""), open: { _ in }, start: {}, refresh: {}) }
}

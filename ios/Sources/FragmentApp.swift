import SwiftUI

@main
struct FragmentApp: App {
    var body: some Scene {
        WindowGroup { FragmentRoot() }
    }
}

@MainActor @Observable
final class Library {
    var fragments: [FragmentSummary] = []
    var error: String?
    var loading = false
    var creating = false
    // Creation and adding the agent are separate API calls. Keep the created
    // chat on a failed join so retrying never creates another empty fragment.
    var unfinishedChat: FragmentSummary?

    var chats: [FragmentSummary] {
        fragments.filter { $0.kind == "chat" && $0.archived != true }.sorted { ($0.previewAt ?? 0) > ($1.previewAt ?? 0) }
    }
    var apps: [FragmentSummary] { fragments.filter { ($0.kind == "app" || $0.kind == "brain") && $0.archived != true } }
    var agent: FragmentSummary? { fragments.first { $0.kind == "agent" } }

    func load(_ api: FragmentAPI) async {
        loading = true
        defer { loading = false }
        do { fragments = try await api.fragments(); error = nil }
        catch { if !Task.isCancelled { self.error = error.localizedDescription } }
    }
}

enum AppSheet: Identifiable {
    case library, settings, app(FragmentSummary)
    var id: String {
        switch self { case .library: "library"; case .settings: "settings"; case .app(let app): app.id }
    }
}

struct FragmentRoot: View {
    @Environment(\.scenePhase) private var scenePhase
    @State private var library = Library()
    @State private var path: [FragmentSummary] = []
    @State private var sheet: AppSheet?
    @State private var homeDraft = ""
    @State private var firstMessage: [String: String] = [:]
    @AppStorage("localServer") private var localServer = "http://localhost:8910"

    private var localURL: URL? {
        #if DEBUG
        let args = ProcessInfo.processInfo.arguments
        let raw = args.firstIndex(of: "--server").flatMap { args.indices.contains($0 + 1) ? args[$0 + 1] : nil } ?? localServer
        guard let url = URL(string: raw), ["http", "https"].contains(url.scheme), url.host != nil,
              url.user == nil, url.password == nil, url.query == nil, url.fragment == nil else { return nil }
        return url
        #else
        return nil
        #endif
    }

    var body: some View {
        Group {
            if let localURL {
                connectedShell(FragmentAPI(baseURL: localURL))
            } else {
                NavigationStack {
                    ContentUnavailableView("Your Fragment, on iPhone", systemImage: "qrcode.viewfinder",
                        description: Text("Device pairing is the next step. This build previews the native app with a local server."))
                    .toolbar { Button("Settings", systemImage: "gearshape") { sheet = .settings } }
                    .sheet(item: $sheet) { _ in SettingsView(localServer: $localServer) }
                }
            }
        }
        .tint(.primary)
    }

    private func connectedShell(_ api: FragmentAPI) -> some View {
        NavigationStack(path: $path) {
            HomeView(library: library, api: api, draft: $homeDraft, open: { fragment in
                if fragment.kind == "chat" { path.append(fragment) }
                else { sheet = .app(fragment) }
            }, start: { await start(api) },
                refresh: { await library.load(api) })
                .toolbar {
                    ToolbarItem(placement: .topBarLeading) {
                        Button("Chats", systemImage: "line.3.horizontal") { sheet = .library }
                    }
                    ToolbarItem(placement: .topBarTrailing) {
                        Button("Settings", systemImage: "gearshape") { sheet = .settings }
                    }
                }
                .navigationDestination(for: FragmentSummary.self) { fragment in
                    ChatView(fragment: fragment, api: api, initialText: firstMessage[fragment.name], consumeInitialText: {
                        firstMessage[fragment.name] = nil
                    }) {
                        sheet = .library
                    }
                    .id(fragment.name)
                }
        }
        .background(FragmentStyle.background)
        .task(id: "\(api.baseURL)|\(scenePhase)") {
            guard scenePhase == .active else { return }
            await library.load(api)
        }
        .onChange(of: path) { _, _ in Task { await library.load(api) } }
        .onChange(of: localServer) { _, _ in path = []; library.fragments = [] }
        .sheet(item: $sheet) { destination in
            switch destination {
            case .settings: SettingsView(localServer: $localServer)
            case .app(let fragment): FragmentWebSheet(fragment: fragment, api: api)
            case .library:
                LibraryView(library: library, api: api) { fragment in
                    sheet = nil
                    if fragment.kind == "chat" { path = [fragment] }
                    else { Task { @MainActor in sheet = .app(fragment) } }
                } newChat: {
                    sheet = nil
                    path = []
                }
            }
        }
    }

    private func start(_ api: FragmentAPI) async {
        guard let agent = library.agent, !library.creating else { return }
        library.creating = true
        defer { library.creating = false }
        do {
            if library.unfinishedChat == nil { library.unfinishedChat = try await api.createChat() }
            guard let chat = library.unfinishedChat else { return }
            try await api.join(agent, to: chat)
            library.unfinishedChat = nil
            firstMessage[chat.name] = homeDraft
            homeDraft = ""
            path.append(chat)
        } catch { library.error = error.localizedDescription }
    }
}

enum FragmentStyle {
    static let background = Color(uiColor: UIColor { $0.userInterfaceStyle == .dark
        ? UIColor(white: 24 / 255, alpha: 1)
        : UIColor(red: 247 / 255, green: 246 / 255, blue: 243 / 255, alpha: 1) })
    static let surface = Color(uiColor: UIColor { $0.userInterfaceStyle == .dark
        ? UIColor(white: 50 / 255, alpha: 1) : UIColor(white: 1, alpha: 1) })
    static let bubble = Color(uiColor: UIColor { $0.userInterfaceStyle == .dark
        ? UIColor(white: 57 / 255, alpha: 1)
        : UIColor(red: 233 / 255, green: 230 / 255, blue: 223 / 255, alpha: 1) })
    static let lilac = Color(red: 168 / 255, green: 139 / 255, blue: 234 / 255)
}

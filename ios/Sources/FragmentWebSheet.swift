import SwiftUI
import WebKit

struct FragmentWebSheet: View {
    @Environment(\.dismiss) private var dismiss
    let fragment: FragmentSummary
    let api: FragmentAPI
    @State private var url: URL?
    @State private var error: String?
    @State private var loading = true

    var body: some View {
        NavigationStack {
            Group {
                if let error {
                    ContentUnavailableView {
                        Label("Couldn’t open this app", systemImage: "wifi.exclamationmark")
                    } description: { Text(error) } actions: {
                        Button("Try again") { Task { await load() } }
                    }
                } else if let url {
                    WebContent(url: url, loading: $loading, error: $error)
                        .overlay { if loading { ProgressView() } }
                } else { ProgressView() }
            }
            .navigationTitle(fragment.displayTitle).navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }
            .task { await load() }
        }
        .presentationDragIndicator(.visible)
    }

    private func load() async {
        error = nil
        loading = true
        do {
            struct Status: Decodable { struct URLs: Decodable { let canonical: URL }; let urls: URLs }
            let status: Status = try await api.request("api/f/\(fragment.name)/status")
            guard ["http", "https"].contains(status.urls.canonical.scheme), status.urls.canonical.host != nil else {
                throw APIError.invalidResponse
            }
            url = status.urls.canonical
        } catch { self.error = error.localizedDescription }
    }
}

private struct WebContent: UIViewRepresentable {
    let url: URL
    @Binding var loading: Bool
    @Binding var error: String?

    func makeCoordinator() -> Coordinator { Coordinator(loading: $loading, error: $error) }
    func makeUIView(context: Context) -> WKWebView {
        let configuration = WKWebViewConfiguration()
        // Each sheet has its own web storage. Native credentials never enter a
        // fragment page; production will use a scoped redemption (docs/ios.md).
        configuration.websiteDataStore = .nonPersistent()
        let web = WKWebView(frame: .zero, configuration: configuration)
        web.navigationDelegate = context.coordinator
        web.allowsBackForwardNavigationGestures = true
        web.load(URLRequest(url: url))
        return web
    }
    func updateUIView(_ uiView: WKWebView, context: Context) {}

    @MainActor final class Coordinator: NSObject, WKNavigationDelegate {
        @Binding var loading: Bool
        @Binding var error: String?
        init(loading: Binding<Bool>, error: Binding<String?>) { _loading = loading; _error = error }
        func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) { loading = false }
        func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
            self.error = error.localizedDescription; loading = false
        }
        func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) {
            self.error = error.localizedDescription; loading = false
        }
        func webView(_ webView: WKWebView, decidePolicyFor navigationAction: WKNavigationAction) async -> WKNavigationActionPolicy {
            guard let url = navigationAction.request.url, ["http", "https", "about"].contains(url.scheme) else {
                return .cancel
            }
            return .allow
        }
    }
}

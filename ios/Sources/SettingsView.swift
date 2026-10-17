import SwiftUI

struct SettingsView: View {
    @Environment(\.dismiss) private var dismiss
    @Binding var localServer: String
    @State private var server = ""

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    HStack(spacing: 14) {
                        Circle().fill(FragmentStyle.lilac.gradient).frame(width: 36, height: 36)
                        VStack(alignment: .leading, spacing: 4) {
                            Text("Fragment").font(.headline)
                            Text("A space for you and your agent.").font(.subheadline).foregroundStyle(.secondary)
                        }
                    }.padding(.vertical, 6)
                }
                Section("Connect your account") {
                    Label("Pair from Fragment settings", systemImage: "qrcode.viewfinder")
                    Text("The planned connection is simple: open Fragment on your computer, choose Settings → Pair iPhone, then scan its QR code here. You’ll approve this device before it can access your account.")
                        .font(.subheadline).foregroundStyle(.secondary)
                    Text("Account pairing isn’t enabled in this local preview.")
                        .font(.footnote).foregroundStyle(.secondary)
                }
                #if DEBUG
                Section {
                    TextField("Server URL", text: $server)
                        .textContentType(.URL).keyboardType(.URL).textInputAutocapitalization(.never).autocorrectionDisabled()
                        .accessibilityIdentifier("serverURL")
                    Button("Connect") { localServer = server.trimmingCharacters(in: .whitespacesAndNewlines); dismiss() }
                } header: { Text("Local development") } footer: {
                    Text("Runs against the local fixture server. No real agent, account, or paid model calls. On a phone, use your Mac’s local hostname.")
                }
                #endif
                Section { LabeledContent("Version", value: "0.1 · Native preview") }
            }
            .navigationTitle("Settings").navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }
            .onAppear { server = localServer }
        }
        .presentationDragIndicator(.visible)
    }
}

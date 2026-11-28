import SwiftUI

@MainActor
struct SettingsView: View {
    @ObservedObject var session: AppSession
    @State private var clearing = false
    @State private var confirmClear = false
    @State private var scope = "settings-" + UUID().uuidString
    var body: some View {
        Form {
            Section("Appearance") {
                Picker("Appearance", selection: Binding(get: { session.appearance }, set: { value in
                    guard !session.changingAppearance else { return }
                    Task {
                        do { try await session.setAppearance(value) }
                        catch { session.present(error: error, scope: scope, title: "Unable to change appearance") }
                    }
                })) {
                    Text("System").tag("system"); Text("Light").tag("light"); Text("Dark").tag("dark")
                }.pickerStyle(.inline).disabled(session.changingAppearance)
            }
            Section("Storage") {
                LabeledContentRow(title: "Save location", value: session.outputLocation)
                LabeledContentRow(title: "Disposable cache", value: nativeBytes(session.cacheBytes))
                Button("Clear cache", role: .destructive) { confirmClear = true }.disabled(clearing)
            }
        }
        .navigationTitle("Settings")
        .navigationBarTitleDisplayMode(.inline)
        .onAppear { session.setPresentationScope(scope); Task { try? await session.refreshCache() } }
        .confirmationDialog("Clear disposable cache?", isPresented: $confirmClear, titleVisibility: .visible) {
            Button("Clear cache", role: .destructive) {
                clearing = true
                Task { defer { clearing = false }; do { try await session.clearCache() } catch { session.present(error: error, scope: scope, title: "Unable to clear cache") } }
            }
            Button("Cancel", role: .cancel) {}
        } message: { Text("Original media, published files and required working resources are preserved.") }
    }
}

struct LabeledContentRow: View {
    let title: String
    let value: String
    var body: some View { HStack(alignment: .firstTextBaseline) { Text(title); Spacer(); Text(value).foregroundColor(.secondary).multilineTextAlignment(.trailing) } }
}

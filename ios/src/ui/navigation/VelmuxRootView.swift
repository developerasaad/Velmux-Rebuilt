import SwiftUI

@MainActor
struct WorkspaceSeed {
    let source: InspectedSource
    let configuration: ConversionConfig
    let operation: Operation
    let previewHandle = MediaPreviewHandle()
}

@MainActor
struct VelmuxRootView: View {
    @ObservedObject var session: AppSession
    @ObservedObject var actions: IosMediaActions
    @State private var seed: WorkspaceSeed?
    @State private var workspaceActive = false
    @State private var preparation: Task<Void, Never>?
    @State private var selection = UUID()
    @State private var preparing = false

    private var activeCount: Int {
        session.jobs.filter { [.queued, .preparing, .processing, .finalizing, .validating, .publishing, .canceling].contains($0.state) }.count
    }

    var body: some View {
        NavigationView {
            Group {
                if session.ready {
                    List {
                        Section("Video") {
                            tool("Video Converter", icon: "arrow.triangle.2.circlepath", operation: .videoConverter)
                            unavailable("Crop Video", icon: "crop")
                            unavailable("Video Cut & Merge", icon: "scissors")
                            unavailable("Extract Audio", icon: "waveform")
                            unavailable("Extract Frames", icon: "photo.on.rectangle")
                            unavailable("Extract Subtitles", icon: "captions.bubble")
                        }
                        Section("Audio") {
                            tool("Audio Converter", icon: "waveform", operation: .audioConverter)
                            unavailable("Audio Cut & Merge", icon: "scissors")
                        }
                        if preparing {
                            HStack { ProgressView(); Text("Inspecting source"); Spacer(); Button("Cancel") { cancelPreparation() } }
                        }
                    }
                    .listStyle(.insetGrouped)
                } else {
                    VStack(spacing: 16) {
                        if session.phase != .failed { ProgressView("Opening Velmux") }
                        if session.phase == .failed { Button("Try again") { Task { await session.start() } } }
                    }.frame(maxWidth: .infinity, maxHeight: .infinity)
                }
            }
            .navigationTitle("Velmux")
            .toolbar {
                ToolbarItemGroup(placement: .navigationBarTrailing) {
                    NavigationLink { JobsView(session: session, actions: actions) } label: {
                        Label(activeCount == 0 ? "Jobs" : "Jobs \(activeCount)", systemImage: "list.bullet.rectangle")
                    }.accessibilityIdentifier("jobs-action")
                    NavigationLink { SettingsView(session: session) } label: {
                        Label("Settings", systemImage: "gearshape")
                    }.accessibilityIdentifier("settings-action")
                }
            }
            .background(
                NavigationLink(isActive: workspaceBinding) {
                    if let seed { ConverterView(seed: seed, session: session, actions: actions, isPresented: workspaceBinding) }
                } label: { EmptyView() }.hidden()
            )
            .onAppear { session.setPresentationScope("home") }
            .onDisappear { if !workspaceActive { cancelPreparation() } }
        }
        .navigationViewStyle(.stack)
        .task { await session.start() }
        .modifier(SessionAlertPresenter(session: session))
    }

    private var workspaceBinding: Binding<Bool> {
        Binding(get: { workspaceActive }, set: { value in
            workspaceActive = value
            if !value, let old = seed {
                old.previewHandle.release()
                seed = nil
                Task { try? await session.releaseInspection(id: old.source.id) }
            }
        })
    }

    private func tool(_ title: String, icon: String, operation: Operation) -> some View {
        Button { select(operation) } label: {
            HStack { Label(title, systemImage: icon); Spacer(); Image(systemName: "chevron.right").font(.caption).foregroundColor(.secondary) }
                .padding(.vertical, 5)
        }.foregroundColor(.primary).disabled(preparing).accessibilityIdentifier(title == "Video Converter" ? "tool-video-converter" : "tool-audio-converter")
    }

    private func unavailable(_ title: String, icon: String) -> some View {
        HStack { Label(title, systemImage: icon); Spacer(); Text("Unavailable").font(.caption) }
            .foregroundColor(.secondary).padding(.vertical, 5)
            .accessibilityElement(children: .combine).accessibilityLabel("\(title), unavailable")
    }

    private func cancelPreparation() { selection = UUID(); preparation?.cancel(); preparation = nil; preparing = false }

    private func select(_ operation: Operation) {
        cancelPreparation()
        let identifier = UUID(); selection = identifier; preparing = true
        actions.selectSource(video: operation == .videoConverter) { selected, error in
            guard selection == identifier else { return }
            if let error { preparing = false; session.present(error: NativePresentationError(error), scope: "home", title: "Unable to select source"); return }
            guard let selected else { preparing = false; return }
            preparation = Task { @MainActor in
                var owned: InspectedSource?
                do {
                    let inspected = try await session.inspect(selected, operation: operation); owned = inspected
                    let configuration = try await session.defaults(sourceId: inspected.id, operation: operation)
                    guard selection == identifier, !Task.isCancelled else { try? await session.releaseInspection(id: inspected.id); return }
                    seed = WorkspaceSeed(source: inspected, configuration: configuration, operation: operation)
                    owned = nil; workspaceActive = true
                } catch {
                    if let owned { try? await session.releaseInspection(id: owned.id) }
                    session.present(error: error, scope: "home", title: "Unable to inspect source")
                }
                if selection == identifier { preparing = false; preparation = nil }
            }
        }
    }
}

struct NativePresentationError: LocalizedError {
    let message: String
    init(_ message: String) { self.message = message }
    var errorDescription: String? { message }
}

@MainActor
struct SessionAlertPresenter: ViewModifier {
    @ObservedObject var session: AppSession
    func body(content: Content) -> some View {
        content.alert(item: Binding(get: { session.alert }, set: { value in
            if value == nil, let current = session.alert { session.dismissAlert(id: current.id) }
        })) { alert in
            Alert(title: Text(alert.title), message: Text(alert.detail), dismissButton: .default(Text("OK")) { session.dismissAlert(id: alert.id) })
        }
    }
}

func nativeBytes(_ count: UInt64) -> String {
    guard let signed = Int64(exactly: count) else { return "\(count) bytes" }
    return ByteCountFormatter.string(fromByteCount: signed, countStyle: .decimal)
}

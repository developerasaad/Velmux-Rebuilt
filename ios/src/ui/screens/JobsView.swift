import SwiftUI

@MainActor
struct JobsView: View {
    @ObservedObject var session: AppSession
    @ObservedObject var actions: IosMediaActions
    @State private var busy = Set<String>()
    @State private var retryKeys: [String: String] = [:]
    @State private var destructive: JobRecord?
    @State private var restart = false
    @State private var adjustment: JobRecord?

    var body: some View {
        List {
            if session.jobs.isEmpty { Text("No jobs").foregroundColor(.secondary) }
            ForEach(session.jobs, id: \.id) { job in
                Section {
                    VStack(alignment: .leading, spacing: 8) {
                        Text(job.fileName).font(.headline)
                        Text(job.sourceName).font(.subheadline).foregroundColor(.secondary)
                        Text(stateLabel(job.state)).font(.subheadline)
                        if active(job.state) {
                            if job.state == .processing, let duration = job.durationUs, duration > 0 {
                                ProgressView(value: min(1, max(0, Double(job.processedUs) / Double(duration))))
                            } else { ProgressView() }
                        }
                        if let detail = job.error { Text(detail).font(.subheadline).foregroundColor(.red).textSelection(.enabled) }
                        if let output = job.output {
                            Text("\(output.displayName) · \(nativeBytes(output.size))").font(.subheadline).foregroundColor(.secondary)
                        } else if job.state == .targetNotMet, let bytes = job.verifiedSize {
                            Text("Verified output: \(nativeBytes(bytes))").font(.subheadline).foregroundColor(.secondary)
                        }
                    }
                    if active(job.state) && job.state != .canceling || [.queued, .paused].contains(job.state) {
                        Button("Cancel") { run(job.id) { try await session.cancelJob(id: job.id) } }
                    }
                    if [.failed, .interrupted].contains(job.state) {
                        if job.verified { Button("Retry saving") { run(job.id) { try await session.retrySaving(id: job.id) } } }
                        Button("Restart from beginning") { restart = true; destructive = job }
                        Button("Discard", role: .destructive) { restart = false; destructive = job }
                    }
                    if job.state == .targetNotMet {
                        Button("Keep output") { run(job.id) { try await session.keepTarget(id: job.id) } }
                        Button("Adjust target") { adjustment = job }
                        Button("Retry target") { retry(job, target: nil) }
                        Button("Discard", role: .destructive) { restart = false; destructive = job }
                    }
                    if let output = job.output, job.state == .completed {
                        Button("Open") { actions.openOutput(locator: output.locator) { if let detail = $0 { session.present(error: NativePresentationError(detail), scope: "jobs", title: "Unable to open output") } } }
                        Button("Share") { actions.shareOutput(locator: output.locator) { if let detail = $0 { session.present(error: NativePresentationError(detail), scope: "jobs", title: "Unable to share output") } } }
                    }
                    if [.completed, .canceled].contains(job.state) { Button("Dismiss") { run(job.id) { try await session.dismissJob(id: job.id) } } }
                }.disabled(busy.contains(job.id))
                    .accessibilityIdentifier("job-" + job.fileName)
            }
        }
        .listStyle(.insetGrouped)
        .navigationTitle("Jobs").navigationBarTitleDisplayMode(.inline)
        .onAppear { session.setPresentationScope("jobs") }
        .confirmationDialog(restart ? "Restart from the beginning?" : "Discard this job?", isPresented: Binding(get: { destructive != nil }, set: { if !$0 { destructive = nil } }), titleVisibility: .visible) {
            if let job = destructive {
                Button(restart ? "Restart" : "Discard", role: .destructive) {
                    destructive = nil
                    run(job.id) {
                        if restart { try await session.restartJob(id: job.id) }
                        else if job.state == .targetNotMet { try await session.discardTarget(id: job.id) }
                        else { try await session.dismissJob(id: job.id) }
                    }
                }
            }
            Button("Cancel", role: .cancel) { destructive = nil }
        } message: { Text("Only unpublished working resources are affected. Original media and published files are preserved.") }
        .sheet(item: $adjustment) { job in TargetAdjustment(session: session) { target in adjustment = nil; retry(job, target: target) } }
    }

    private func run(_ id: String, action: @escaping () async throws -> Void) {
        guard !busy.contains(id) else { return }; busy.insert(id)
        Task { defer { busy.remove(id) }; do { try await action() } catch { session.present(error: error, scope: "jobs", title: "Unable to update job") } }
    }
    private func retry(_ job: JobRecord, target: TargetSize?) {
        let identity = job.id + ":" + (target.map { "\($0.amountMilli):\($0.unit)" } ?? "retry")
        let key = retryKeys[identity] ?? UUID().uuidString; retryKeys[identity] = key
        run(job.id) { _ = try await session.retryTarget(id: job.id, newTarget: target, key: key) }
    }
    private func active(_ state: JobState) -> Bool { [.preparing, .processing, .finalizing, .validating, .publishing, .canceling].contains(state) }
    private func stateLabel(_ state: JobState) -> String {
        switch state {
        case .queued: return "Queued"; case .preparing: return "Preparing"; case .processing: return "Processing"
        case .finalizing: return "Finalizing"; case .validating: return "Validating"; case .publishing: return "Saving"
        case .canceling: return "Canceling"; case .completed: return "Completed"; case .failed: return "Failed"
        case .canceled: return "Canceled"; case .interrupted: return "Interrupted"; case .paused: return "Paused"
        case .targetNotMet: return "Target Not Met"
        }
    }
}

// A presentation identity does not replace the authoritative Rust job identity.
extension JobRecord: Identifiable {}

@MainActor
private struct TargetAdjustment: View {
    @ObservedObject var session: AppSession
    let onApply: (TargetSize) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var value = ""
    @State private var unit = TargetSizeUnit.megabytes
    @State private var validation: String?
    var body: some View {
        NavigationView {
            Form {
                Section("Target size") {
                    TextField("Positive size", text: $value).keyboardType(.decimalPad)
                    Picker("Unit", selection: $unit) { Text("KB").tag(TargetSizeUnit.kilobytes); Text("MB").tag(TargetSizeUnit.megabytes); Text("GB").tag(TargetSizeUnit.gigabytes) }
                    Text("KB = 1,000 bytes. MB = 1,000,000 bytes.").font(.footnote).foregroundColor(.secondary)
                    if let validation { Text(validation).foregroundColor(.red) }
                }
                Button("Adjust and retry") {
                    do { onApply(try session.parseTarget(value: value, unit: unit)) }
                    catch { validation = session.errorDetail(error) }
                }.disabled(value.isEmpty)
            }
            .navigationTitle("Adjust target").navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } } }
        }
    }
}

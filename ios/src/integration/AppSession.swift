import Combine
import Foundation

struct SessionAlert: Identifiable {
    let id = UUID()
    let scope: String
    let title: String
    let detail: String
}

enum SessionPhase: Equatable {
    case idle
    case starting
    case ready
    case failed
}

private enum SessionFailure: LocalizedError {
    case notReady
    case requestsBusy

    var errorDescription: String? {
        switch self {
        case .notReady: return "The application is not ready. Retry initialization before continuing."
        case .requestsBusy: return "Several requests are still finishing. Wait a moment and try again."
        }
    }
}

/// A queued request retains its Rust token until the actual FFI call returns.
/// Canceling a Swift task never frees a descriptor or interrupts native cleanup.
private final class SessionRequest: @unchecked Sendable {
    let id = UUID()
    let token = CancellationToken()
    func cancel() { token.cancel() }
}

private final class SessionJobObserver: JobObserver, @unchecked Sendable {
    private let lock = NSLock()
    private weak var session: AppSession?

    func connect(_ session: AppSession) {
        lock.lock()
        self.session = session
        lock.unlock()
    }

    func onJobsChanged() {
        lock.lock()
        let receiver = session
        lock.unlock()
        DispatchQueue.main.async { [weak receiver] in receiver?.jobsChanged() }
    }
}

/// The application owns this singleton. Views own navigation and transient
/// workspace state; Rust owns accepted jobs, compatibility and all media policy.
@MainActor
final class AppSession: ObservableObject {
    static let shared = AppSession()

    @Published private(set) var phase: SessionPhase = .idle
    @Published private(set) var ready = false
    @Published private(set) var jobs: [JobRecord] = []
    @Published private(set) var appearance = "system"
    @Published private(set) var outputLocation = ""
    @Published private(set) var cacheBytes: UInt64 = 0
    @Published private(set) var runtimeError: String?
    @Published private(set) var alert: SessionAlert?

    private let nativeServices: IosServices
    private let mediaServices: IosMediaServices
    private let mediaProvider: IosMediaProvider
    private let engine: Engine
    private let observer = SessionJobObserver()
    private let workers: OperationQueue
    private let previewWorker: OperationQueue
    private var mediaInitialized = false
    private var observerInstalled = false
    private var starting = false
    private var requests: [UUID: SessionRequest] = [:]
    private var previewRequests: [String: SessionRequest] = [:]
    private var jobsDirty = false
    private var refreshingJobs = false
    private var lastJobsError: String?
    private var presentationScope = "application"

    private init() {
        let native = IosServices()
        let media = IosMediaServices()
        nativeServices = native
        mediaServices = media
        mediaProvider = IosMediaProvider(services: media)
        engine = Engine(provider: native)
        workers = OperationQueue()
        workers.name = "com.velmux.mobile.requests"
        workers.qualityOfService = .userInitiated
        workers.maxConcurrentOperationCount = 4
        previewWorker = OperationQueue()
        previewWorker.name = "com.velmux.mobile.preview-requests"
        previewWorker.qualityOfService = .userInitiated
        previewWorker.maxConcurrentOperationCount = 1
        observer.connect(self)
    }

    /// Initialization can be retried without creating another engine or media
    /// runtime. Each successful stage is recorded before the next awaited call.
    func start() async {
        guard !starting, !ready else { return }
        starting = true
        phase = .starting
        defer { starting = false }
        do {
            if !mediaInitialized {
                let provider = mediaProvider
                try await perform { core, _ in try core.initializeMedia(provider: provider) }
                mediaInitialized = true
            }
            if !observerInstalled {
                let observer = observer
                try await perform { core, _ in try core.observeJobs(observer: observer) }
                observerInstalled = true
            }
            let state = try await perform { core, _ in
                (try core.jobs(), try core.appearance(), try core.outputLocation(),
                 try core.cacheBytes(), try core.runtimeError())
            }
            jobs = state.0
            appearance = state.1
            outputLocation = state.2
            cacheBytes = state.3
            runtimeError = state.4
            ready = true
            phase = .ready
            if alert?.scope == "initialization" { alert = nil }
            if jobsDirty { jobsChanged() }
        } catch {
            ready = false
            phase = .failed
            present(error: error, scope: "initialization", title: "Unable to start Velmux")
        }
    }

    func setPresentationScope(_ scope: String) {
        guard presentationScope != scope else { return }
        presentationScope = scope
        if let current = alert,
           current.scope != "application", current.scope != "initialization",
           current.scope != scope { alert = nil }
    }

    func present(error: Error, scope: String, title: String = "Unable to continue") {
        if error is CancellationError { return }
        if case EngineError.Cancelled = error { return }
        guard scope == presentationScope || scope == "application" || scope == "initialization" else { return }
        alert = SessionAlert(scope: scope, title: title, detail: Self.detail(error))
    }

    func dismissAlert(id: UUID) {
        if alert?.id == id { alert = nil }
    }

    func errorDetail(_ error: Error) -> String { Self.detail(error) }

    func catalog(sourceId: String, operation: Operation) throws -> [FormatOption] {
        try requireReady()
        return try engine.mediaCatalogForSource(id: sourceId, operation: operation)
    }

    func choices(sourceId: String, configuration: ConversionConfig) throws -> ConfigurationChoices {
        try requireReady()
        return try engine.configurationChoices(id: sourceId, configuration: configuration)
    }

    func parseTarget(value: String, unit: TargetSizeUnit) throws -> TargetSize {
        try engine.parseTargetSize(value: value, unit: unit)
    }

    func inspect(_ selected: IosSelectedSource, operation: Operation) async throws -> InspectedSource {
        try requireReady()
        return try await perform(discardWhenCanceled: true, onAbandoned: { core, source in
            try? core.releaseInspection(id: source.id)
        }) { core, token in
            try core.inspectSource(locator: selected.locator, displayName: selected.displayName,
                                   operation: operation, cancellation: token)
        }
    }

    func releaseInspection(id: String) async throws {
        previewRequests[id]?.cancel()
        try await perform(cancelBeforeStart: false) { core, _ in try core.releaseInspection(id: id) }
    }

    func defaults(sourceId: String, operation: Operation) async throws -> ConversionConfig {
        try requireReady()
        return try await perform { core, _ in
            var configuration = try core.defaultConfiguration(id: sourceId, operation: operation)
            // Temporary migration adapter: the retained M2 quality/bitrate
            // controls use the preserved legacy model until full M3 UI wiring.
            configuration.compression = nil
            return configuration
        }
    }

    func validate(sourceId: String, configuration: ConversionConfig) async throws {
        try requireReady()
        try await perform { core, _ in try core.validateConfiguration(id: sourceId, configuration: configuration) }
    }

    func submit(sourceId: String, key: String, configuration: ConversionConfig) async throws -> JobRecord {
        try requireReady()
        // A returned accepted job remains authoritative even if the caller's
        // task was canceled just after acceptance. Never cancel that job here.
        let accepted = try await perform { core, token in
            try core.submitConversion(id: sourceId, submissionKey: key,
                                      configuration: configuration, cancellation: token)
        }
        jobsChanged()
        return accepted
    }

    func preparePreview(sourceId: String, startUs: Int64) async throws -> PreviewResult {
        try requireReady()
        previewRequests[sourceId]?.cancel()
        let request = SessionRequest()
        previewRequests[sourceId] = request
        defer {
            if previewRequests[sourceId]?.id == request.id { previewRequests.removeValue(forKey: sourceId) }
        }
        // Serial presentation requests ensure an abandoned old preview is
        // retired before a replacement begins. Other engine calls use workers.
        return try await perform(request: request, queue: previewWorker,
                                 discardWhenCanceled: true, onAbandoned: { core, _ in
            try? core.releasePreview(id: sourceId)
        }) { core, token in
            try core.preparePreview(id: sourceId, startUs: startUs, cancellation: token)
        }
    }

    func releasePreview(sourceId: String) async throws {
        previewRequests[sourceId]?.cancel()
        try await perform(queue: previewWorker, cancelBeforeStart: false) { core, _ in
            try core.releasePreview(id: sourceId)
        }
    }

    func cancelJob(id: String) async throws {
        try requireReady()
        try await perform { core, _ in try core.cancelJob(id: id) }
        jobsChanged()
    }

    func restartJob(id: String) async throws {
        try requireReady()
        try await perform { core, _ in try core.restartJob(id: id) }
        jobsChanged()
    }

    func retrySaving(id: String) async throws {
        try requireReady()
        try await perform { core, _ in try core.retryPublication(id: id) }
        jobsChanged()
    }

    func dismissJob(id: String) async throws {
        try requireReady()
        try await perform { core, _ in try core.dismissJob(id: id) }
        jobsChanged()
    }

    func keepTarget(id: String) async throws {
        try requireReady()
        try await perform { core, _ in try core.keepTargetOutput(id: id) }
        jobsChanged()
    }

    @discardableResult
    func retryTarget(id: String, newTarget: TargetSize?, key: String) async throws -> JobRecord {
        try requireReady()
        let accepted = try await perform { core, token in
            try core.resubmitTarget(id: id, newTarget: newTarget, submissionKey: key, cancellation: token)
        }
        jobsChanged()
        return accepted
    }

    func discardTarget(id: String) async throws {
        try requireReady()
        try await perform { core, _ in try core.discardTargetOutput(id: id) }
        jobsChanged()
    }

    func setAppearance(_ value: String) async throws {
        try requireReady()
        let persisted = try await perform { core, _ in
            try core.setAppearance(value: value)
            return try core.appearance()
        }
        appearance = persisted
    }

    func refreshCache() async throws {
        try requireReady()
        cacheBytes = try await perform { core, _ in try core.cacheBytes() }
    }

    func clearCache() async throws {
        try requireReady()
        cacheBytes = try await perform { core, _ in
            try core.clearCache()
            return try core.cacheBytes()
        }
    }

    /// Cancellation concerns transient requests only. Rust-owned accepted jobs
    /// require the explicit cancelJob action and survive workspace disposal.
    func cancelAllRequests() {
        for request in requests.values { request.cancel() }
    }

    fileprivate func jobsChanged() {
        jobsDirty = true
        guard ready, !refreshingJobs else { return }
        refreshingJobs = true
        Task { [weak self] in
            guard let self else { return }
            defer { self.refreshingJobs = false }
            while self.jobsDirty {
                self.jobsDirty = false
                do {
                    let state = try await self.perform { core, _ in (try core.jobs(), try core.runtimeError()) }
                    self.jobs = state.0
                    self.runtimeError = state.1
                    self.lastJobsError = nil
                } catch {
                    let detail = Self.detail(error)
                    if detail != self.lastJobsError {
                        self.lastJobsError = detail
                        self.present(error: error, scope: "application", title: "Unable to update Jobs")
                    }
                }
            }
        }
    }

    private func requireReady() throws {
        guard ready else { throw SessionFailure.notReady }
    }

    private func perform<Value>(
        request: SessionRequest = SessionRequest(),
        queue: OperationQueue? = nil,
        cancelBeforeStart: Bool = true,
        discardWhenCanceled: Bool = false,
        onAbandoned: (@Sendable (Engine, Value) -> Void)? = nil,
        action: @escaping @Sendable (Engine, CancellationToken) throws -> Value
    ) async throws -> Value {
        // Cleanup must remain available when the admission limit is reached.
        // The queues still bound simultaneous native work.
        guard !cancelBeforeStart || requests.count < 32 else { throw SessionFailure.requestsBusy }
        requests[request.id] = request
        defer { requests.removeValue(forKey: request.id) }
        let core = engine
        let executionQueue = queue ?? workers
        return try await withTaskCancellationHandler(operation: {
            try await withCheckedThrowingContinuation { continuation in
                executionQueue.addOperation {
                    do {
                        if cancelBeforeStart && request.token.isCancelled() { throw EngineError.Cancelled }
                        let value = try action(core, request.token)
                        if discardWhenCanceled && request.token.isCancelled() {
                            onAbandoned?(core, value)
                            throw EngineError.Cancelled
                        }
                        continuation.resume(returning: value)
                    } catch {
                        continuation.resume(throwing: error)
                    }
                }
            }
        }, onCancel: { request.cancel() })
    }

    private static func detail(_ error: Error) -> String {
        let message: String
        switch error {
        case EngineError.InvalidStorageRoot:
            message = "Velmux could not access its private application storage. Retry after storage becomes available."
        case EngineError.SourceAccess(let detail), EngineError.UnsupportedMedia(let detail),
             EngineError.InvalidMedia(let detail), EngineError.Configuration(let detail),
             EngineError.Decoding(let detail), EngineError.Encoding(let detail),
             EngineError.Filtering(let detail), EngineError.Muxing(let detail),
             EngineError.Storage(let detail), EngineError.Recovery(let detail),
             EngineError.Platform(let detail), EngineError.Internal(let detail):
            message = detail
        default:
            message = error.localizedDescription
        }
        let bounded = String(message.trimmingCharacters(in: .whitespacesAndNewlines).prefix(1000))
        return bounded.isEmpty ? "The operation could not be completed. Try again." : bounded
    }
}

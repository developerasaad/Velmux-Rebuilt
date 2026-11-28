import UIKit
import XCTest
@testable import Velmux

final class FoundationIntegrationTests: XCTestCase {
    @MainActor
    func testNativeApplicationUsesOneLiveProcessSessionAndPreservesStorageIdentity() async throws {
        let first = AppSession.shared
        await first.start()
        let deadline = ProcessInfo.processInfo.systemUptime + 45
        while !first.ready, first.phase != .failed, ProcessInfo.processInfo.systemUptime < deadline {
            try await Task.sleep(nanoseconds: 20_000_000)
        }
        let second = AppSession.shared
        await second.start()
        XCTAssertTrue(first === second)
        XCTAssertTrue(first.ready)
        XCTAssertEqual(first.phase, .ready)
        let root = try XCTUnwrap(UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
            .flatMap(\.windows).first(where: \.isKeyWindow)?.rootViewController)
        root.loadViewIfNeeded()
        XCTAssertNotNil(root.view.window)
        XCTAssertEqual(Bundle.main.bundleIdentifier, "com.velmux.mobile")
        XCTAssertEqual(Bundle.main.object(forInfoDictionaryKey: "UIFileSharingEnabled") as? Bool, true)
        XCTAssertEqual(Bundle.main.object(forInfoDictionaryKey: "LSSupportsOpeningDocumentsInPlace") as? Bool, true)
        XCTAssertNil(Bundle.main.object(forInfoDictionaryKey: "CADisableMinimumFrameDurationOnPhone"))
        XCTAssertEqual(first.outputLocation, "Files app → Velmux → Velmux")
        XCTAssertTrue(IosServices().storageRoot().hasSuffix("/Library/Application Support/Velmux"))
    }

    func testDirectGeneratedBindingsLinkRealFfmpegAndDeliverWorkerCallbacks() async throws {
        let services = IosServices()
        let engine = Engine(provider: services)
        let first = try await worker { try engine.diagnostics(cancellation: CancellationToken()) }
        let second = try await worker { try engine.diagnostics(cancellation: CancellationToken()) }
        XCTAssertTrue(first.ffmpegVersion.hasPrefix("7.1."))
        XCTAssertGreaterThan(first.encoderCount, 0)
        XCTAssertGreaterThan(first.decoderCount, 0)
        XCTAssertGreaterThan(first.muxerCount, 0)
        XCTAssertGreaterThan(first.demuxerCount, 0)
        XCTAssertGreaterThan(first.filterCount, 100)
        XCTAssertEqual(first.storageRoot, services.storageRoot())
        XCTAssertEqual(second.sequence, first.sequence + 1)
        XCTAssertEqual(services.callbackCount, 2)
        XCTAssertFalse(services.lastCallbackWasMainThread)
        let canceled = CancellationToken()
        canceled.cancel()
        do {
            _ = try await worker { try engine.diagnostics(cancellation: canceled) }
            XCTFail("Pre-call cancellation must reject the Rust request")
        } catch EngineError.Cancelled {}
        XCTAssertEqual(services.callbackCount, 2)
    }

    func testInvalidStorageAndMidCallbackCancellationAreTyped() throws {
        let invalid = Engine(provider: InvalidStorageProvider())
        XCTAssertThrowsError(try invalid.diagnostics(cancellation: CancellationToken())) { error in
            guard case EngineError.InvalidStorageRoot = error else { return XCTFail("Unexpected error: \(error)") }
        }
        let token = CancellationToken()
        let provider = CancelingDiagnosticProvider(token: token)
        let engine = Engine(provider: provider)
        XCTAssertThrowsError(try engine.diagnostics(cancellation: token)) { error in
            guard case EngineError.Cancelled = error else { return XCTFail("Unexpected error: \(error)") }
        }
        XCTAssertEqual(provider.calls, 1)
    }

    @MainActor
    func testScopedAlertsIgnoreObsoleteErrorsAndDismissOnlyTheDisplayedIdentity() async {
        let session = AppSession.shared
        await session.start()
        session.setPresentationScope("test-first")
        session.present(error: EngineError.Configuration(detail: "First error"), scope: "test-first")
        let original = session.alert?.id
        session.setPresentationScope("test-second")
        XCTAssertNil(session.alert)
        session.present(error: EngineError.Configuration(detail: "Late error"), scope: "test-first")
        XCTAssertNil(session.alert)
        session.present(error: EngineError.Configuration(detail: "Current error"), scope: "test-second")
        let current = session.alert?.id
        if let original { session.dismissAlert(id: original) }
        XCTAssertEqual(session.alert?.id, current)
        if let current { session.dismissAlert(id: current) }
        XCTAssertNil(session.alert)
        session.setPresentationScope("home")
    }

    private func worker<Value>(_ action: @escaping () throws -> Value) async throws -> Value {
        try await withCheckedThrowingContinuation { continuation in
            DispatchQueue.global(qos: .userInitiated).async { continuation.resume(with: Result(catching: action)) }
        }
    }
}

private final class InvalidStorageProvider: NativeProvider, @unchecked Sendable {
    func storageRoot() -> String { "" }
    func platformName() -> String { "iOS" }
    func onDiagnostic(sequence: UInt64) { XCTFail("Invalid storage must reject the request before callbacks") }
}

private final class CancelingDiagnosticProvider: NativeProvider, @unchecked Sendable {
    let token: CancellationToken
    private(set) var calls = 0
    init(token: CancellationToken) { self.token = token }
    func storageRoot() -> String { IosServices().storageRoot() }
    func platformName() -> String { "iOS" }
    func onDiagnostic(sequence: UInt64) { calls += 1; token.cancel() }
}

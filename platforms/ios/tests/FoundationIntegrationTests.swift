import XCTest
import UIKit
import VelmuxUI
@testable import Velmux

final class FoundationIntegrationTests: XCTestCase {
    @MainActor
    func testApplicationHostUsesLiveSharedSession() throws {
        let host = try XCTUnwrap(UIApplication.shared.delegate as? AppDelegate)
        XCTAssertNotNil(host.window?.rootViewController)
        let session = try XCTUnwrap(host.session)
        let report = try session.inspect()
        XCTAssertTrue(report.ffmpegVersion.hasPrefix("7.1."))
        XCTAssertGreaterThan(report.filterCount, 100)
    }

    func testGeneratedBindingsAndLinkedFfmpeg() throws {
        let services = IosServices()
        let session = FoundationSession(services: services)
        defer {
            session.close()
            session.close()
        }
        let first = try session.inspect()
        let second = try session.inspect()
        XCTAssertFalse(first.ffmpegVersion.isEmpty)
        XCTAssertGreaterThan(first.encoderCount, 0)
        XCTAssertGreaterThan(first.decoderCount, 0)
        XCTAssertGreaterThan(first.muxerCount, 0)
        XCTAssertGreaterThan(first.demuxerCount, 0)
        XCTAssertGreaterThan(first.filterCount, 0)
        XCTAssertEqual(first.storageRoot, services.storageRoot())
        XCTAssertEqual(first.sequence + 1, second.sequence)
        XCTAssertEqual(services.callbackCount, 2)
        XCTAssertTrue(try session.verifyCancelledRequest())
        XCTAssertEqual(services.callbackCount, 2)
    }

    func testNativeCallbackFromWorkerThread() {
        let completed = expectation(description: "Worker reached native provider")
        let services = IosServices()
        let session = FoundationSession(services: services)
        DispatchQueue.global(qos: .userInitiated).async {
            defer {
                session.close()
                completed.fulfill()
            }
            do {
                let report = try session.inspect()
                XCTAssertGreaterThan(report.encoderCount, 0)
                XCTAssertEqual(services.callbackCount, 1)
                XCTAssertFalse(services.lastCallbackWasMainThread)
            } catch {
                XCTFail("Worker diagnostic failed: \(error)")
            }
        }
        wait(for: [completed], timeout: 30)
    }

    func testInvalidNativeStorageIsTypedAndCatchable() throws {
        let session = FoundationSession(services: InaccessibleStorageServices())
        defer { session.close() }
        XCTAssertTrue(try session.verifyInvalidStorageRequest())
        XCTAssertThrowsError(try session.inspect())
    }

    func testClosedSessionErrorIsCatchable() {
        let session = FoundationSession(services: IosServices())
        session.close()
        XCTAssertThrowsError(try session.inspect())
    }
}

private final class InaccessibleStorageServices: NSObject, PlatformServices {
    func storageRoot() -> String { "" }
    func platformName() -> String { "iOS" }
    func onDiagnostic(sequence: Int64) { XCTFail("Invalid storage must fail before a diagnostic callback") }
}

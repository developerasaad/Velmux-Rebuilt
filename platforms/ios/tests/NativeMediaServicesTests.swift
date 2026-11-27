import AVFoundation
import AVKit
import Darwin
import UIKit
import XCTest
import VelmuxUI
@testable import Velmux

final class NativeMediaServicesTests: XCTestCase {
    private var root: URL!
    private var services: IosMediaServices!

    override func setUpWithError() throws {
        root = FileManager.default.temporaryDirectory.appendingPathComponent("velmux-native-tests-" + UUID().uuidString, isDirectory: true)
        services = IosMediaServices(
            operationalRoot: root.appendingPathComponent("support", isDirectory: true),
            cacheDirectory: root.appendingPathComponent("cache", isDirectory: true),
            outputDirectory: root.appendingPathComponent("documents/Velmux", isDirectory: true)
        )
        try FileManager.default.createDirectory(at: services.operationalRoot, withIntermediateDirectories: true)
    }

    override func tearDownWithError() throws {
        services = nil
        if let root { try FileManager.default.removeItem(at: root) }
    }

    private func workingFile(_ name: String = "result.tmp", bytes: Data = Data("verified working bytes".utf8)) throws -> URL {
        let url = services.operationalRoot.appendingPathComponent(name)
        try bytes.write(to: url, options: .withoutOverwriting)
        return url
    }

    func testBookmarkReopensOriginalWithoutStagingAndTransfersDescriptor() throws {
        let bytes = Data("the original must remain unchanged".utf8)
        let source = root.appendingPathComponent("original.dat")
        try bytes.write(to: source)
        let selected = try IosMediaServices.selectedSource(url: source)
        XCTAssertTrue(selected.locator.hasPrefix("bookmark:"))
        let opened = try services.openSource(locator: selected.locator)
        XCTAssertEqual(opened.offset, 0)
        XCTAssertEqual(opened.length, Int64(bytes.count))
        var actual = [UInt8](repeating: 0, count: bytes.count)
        let count = actual.count
        XCTAssertEqual(Darwin.read(opened.fd, &actual, count), count)
        XCTAssertEqual(Data(actual), bytes)
        // This is the Rust owner's close, followed by native scope release.
        XCTAssertEqual(Darwin.close(opened.fd), 0)
        services.releaseSource(leaseId: opened.leaseId)
        services.releaseSource(leaseId: opened.leaseId)
        XCTAssertEqual(try Data(contentsOf: source), bytes)
        XCTAssertFalse(FileManager.default.fileExists(atPath: services.cacheDirectory.path))

        let restarted = IosMediaServices(operationalRoot: services.operationalRoot, cacheDirectory: services.cacheDirectory, outputDirectory: services.outputDirectory)
        let reopened = try restarted.openSource(locator: selected.locator)
        XCTAssertEqual(Darwin.close(reopened.fd), 0)
        restarted.releaseSource(leaseId: reopened.leaseId)
    }

    func testRetiringInlineBookmarkDoesNotDeleteOriginalOrReleaseAnActiveLease() throws {
        let bytes = Data("independently owned active document bytes".utf8)
        let source = root.appendingPathComponent("still-owned-original.dat")
        try bytes.write(to: source, options: .withoutOverwriting)
        let selected = try IosMediaServices.selectedSource(url: source)
        try services.retainSourceAccess(locator: selected.locator)
        let opened = try services.openSource(locator: selected.locator)
        defer {
            Darwin.close(opened.fd)
            services.releaseSource(leaseId: opened.leaseId)
        }
        services.releaseSourceAccess(locator: selected.locator)
        services.releaseSourceAccess(locator: selected.locator)
        services.reconcileSourceAccess(retainedLocators: [selected.locator])
        services.reconcileSourceAccess(retainedLocators: [])
        // Unknown or plain-file locators cannot become deletion instructions.
        services.releaseSourceAccess(locator: source.absoluteString)
        services.releaseSourceAccess(locator: "bookmark:invalid")
        var actual = [UInt8](repeating: 0, count: bytes.count)
        let count = actual.count
        XCTAssertEqual(Darwin.read(opened.fd, &actual, count), count)
        XCTAssertEqual(Data(actual), bytes)
        XCTAssertEqual(try Data(contentsOf: source), bytes)
        XCTAssertEqual(try FileManager.default.contentsOfDirectory(atPath: services.operationalRoot.path), [])
        XCTAssertFalse(FileManager.default.fileExists(atPath: services.cacheDirectory.path))
    }

    func testPublicationSurvivesInterruptedCommitAndNeverDeletesUserOutput() throws {
        let working = try workingFile()
        let bytes = try Data(contentsOf: working)
        let reservation = try services.reserveOutput(jobId: "stable-job-1", fileName: "video.mp4", mimeType: "video/mp4")
        let repeated = try services.reserveOutput(jobId: "stable-job-1", fileName: "video.mp4", mimeType: "video/mp4")
        XCTAssertEqual(repeated.locator, reservation.locator)
        XCTAssertNil(try services.publicationStatus(reservation: reservation).output)
        let published = try services.publishOutput(reservation: reservation, workingPath: working.path, expectedSize: Int64(bytes.count))
        let outputURL = try XCTUnwrap(URL(string: published.locator))
        XCTAssertEqual(try Data(contentsOf: outputURL), bytes)
        XCTAssertFalse(FileManager.default.fileExists(atPath: working.path))

        // Recreate the durable pre-rename record to represent death after rename.
        let identifier = String(reservation.locator.dropFirst("velmux-reservation:".count))
        let manifest = services.operationalRoot.appendingPathComponent("publication-reservations/" + identifier + ".json")
        var record = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: manifest)) as? [String: Any])
        record["published"] = false
        try JSONSerialization.data(withJSONObject: record).write(to: manifest, options: .atomic)
        let restarted = IosMediaServices(operationalRoot: services.operationalRoot, cacheDirectory: services.cacheDirectory, outputDirectory: services.outputDirectory)
        let recovered = try XCTUnwrap(restarted.publicationStatus(reservation: reservation).output)
        XCTAssertEqual(recovered.locator, published.locator)
        XCTAssertEqual(try restarted.publishOutput(reservation: reservation, workingPath: working.path, expectedSize: Int64(bytes.count)).locator, published.locator)
        restarted.discardReservation(reservation: reservation)
        XCTAssertEqual(try Data(contentsOf: outputURL), bytes)
    }

    func testDestinationCollisionAndInvalidSizePreserveExistingFilesAndWorkingResult() throws {
        let reservation = try services.reserveOutput(jobId: "collision-job", fileName: "video.mp4", mimeType: "video/mp4")
        let originalBytes = Data("existing user file".utf8)
        let occupied = services.outputDirectory.appendingPathComponent("video.mp4")
        try originalBytes.write(to: occupied, options: .withoutOverwriting)
        let working = try workingFile()
        let expected = try Data(contentsOf: working)
        XCTAssertThrowsError(try services.publishOutput(reservation: reservation, workingPath: working.path, expectedSize: Int64(expected.count + 1)))
        XCTAssertEqual(try Data(contentsOf: working), expected)
        let result = try services.publishOutput(reservation: reservation, workingPath: working.path, expectedSize: Int64(expected.count))
        XCTAssertEqual(result.displayName, "video (1).mp4")
        XCTAssertEqual(try Data(contentsOf: occupied), originalBytes)
        XCTAssertEqual(try Data(contentsOf: XCTUnwrap(URL(string: result.locator))), expected)
    }

    func testDiscardRemovesOnlyUnpublishedOwnershipMetadata() throws {
        let source = root.appendingPathComponent("user-original.mov")
        try Data("original".utf8).write(to: source)
        let reservation = try services.reserveOutput(jobId: "discard-job", fileName: "../unsafe/name.mp4", mimeType: "video/mp4")
        let working = try workingFile()
        services.discardReservation(reservation: reservation)
        services.discardReservation(reservation: reservation)
        XCTAssertTrue(FileManager.default.fileExists(atPath: source.path))
        XCTAssertTrue(FileManager.default.fileExists(atPath: working.path))
        XCTAssertEqual(try FileManager.default.contentsOfDirectory(atPath: services.outputDirectory.path), [])
        XCTAssertThrowsError(try services.publishOutput(reservation: reservation, workingPath: source.path, expectedSize: 8))
    }

    func testPublicationRejectsOriginalOutsideWorkingRoots() throws {
        let original = root.appendingPathComponent("original.wav")
        try Data("original".utf8).write(to: original)
        let reservation = try services.reserveOutput(jobId: "unsafe-job", fileName: "audio.wav", mimeType: "audio/wav")
        XCTAssertThrowsError(try services.publishOutput(reservation: reservation, workingPath: original.path, expectedSize: 8))
        XCTAssertEqual(try Data(contentsOf: original), Data("original".utf8))
    }

    func testInterruptedInitialReservationAndLongNameCollisionRecoverSafely() throws {
        let longName = String(repeating: "a", count: 180) + ".mp4"
        let first = try services.reserveOutput(jobId: "long-name-first", fileName: longName, mimeType: "video/mp4")
        XCTAssertEqual(first.displayName, longName, "A valid maximum-length stem must retain its media extension")
        let identifier = String(first.locator.dropFirst("velmux-reservation:".count))
        let manifest = services.operationalRoot.appendingPathComponent("publication-reservations/" + identifier + ".json")
        try FileManager.default.removeItem(at: manifest)
        let recovered = try services.reserveOutput(jobId: "long-name-first", fileName: longName, mimeType: "video/mp4")
        XCTAssertEqual(first.locator, recovered.locator)
        let second = try services.reserveOutput(jobId: "long-name-second", fileName: longName, mimeType: "video/mp4")
        let working = try workingFile()
        let size = Int64(try Data(contentsOf: working).count)
        let result = try services.publishOutput(reservation: second, workingPath: working.path, expectedSize: size)
        XCTAssertTrue(result.displayName.contains(" (1).mp4"))
        XCTAssertEqual(try services.publicationStatus(reservation: second).output?.locator, result.locator)
    }

    @MainActor
    func testNativeAudioPreviewLoadsRealSamplesAndSeeks() throws {
        let fixture = root.appendingPathComponent("tone.wav")
        try Self.waveFixture().write(to: fixture)
        let actions = IosMediaActions { nil }
        let controller = actions.createPreview(locator: fixture.absoluteString) {
            XCTFail("Valid PCM audio should play natively")
            return
        }
        defer { actions.releasePreview(controller: controller) }
        controller.loadViewIfNeeded()
        let playback = try XCTUnwrap(controller.children.first as? AVPlayerViewController)
        let attached = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in playback.player != nil }, object: nil)
        wait(for: [attached], timeout: 15)
        XCTAssertTrue(playback.showsPlaybackControls)
        let player = try XCTUnwrap(playback.player)
        let item = try XCTUnwrap(player.currentItem)
        let ready = expectation(description: "Native player decoded the WAV fixture")
        let observation = item.observe(\.status, options: [.initial, .new]) { item, _ in
            if item.status == .readyToPlay { ready.fulfill() }
        }
        wait(for: [ready], timeout: 10)
        observation.invalidate()
        XCTAssertEqual(item.status, .readyToPlay)
        let sought = expectation(description: "Native player completed a seek")
        player.seek(to: CMTime(seconds: 0.25, preferredTimescale: 8_000), toleranceBefore: .zero, toleranceAfter: .zero) { finished in
            XCTAssertTrue(finished)
            sought.fulfill()
        }
        wait(for: [sought], timeout: 10)
        XCTAssertEqual(player.currentTime().seconds, 0.25, accuracy: 0.025)
    }

    @MainActor
    func testReleasedPreviewNeverAttachesLatePlayerOrReportsUnsupported() throws {
        let fixture = root.appendingPathComponent("released-tone.wav")
        try Self.waveFixture().write(to: fixture, options: .withoutOverwriting)
        let actions = IosMediaActions { nil }
        let unsupported = expectation(description: "Released preview must suppress late errors")
        unsupported.isInverted = true
        let controller = actions.createPreview(locator: fixture.absoluteString) { unsupported.fulfill() }
        controller.loadViewIfNeeded()
        let playback = try XCTUnwrap(controller.children.first as? AVPlayerViewController)
        actions.releasePreview(controller: controller)
        actions.releasePreview(controller: controller)
        wait(for: [unsupported], timeout: 0.5)
        XCTAssertNil(playback.player)
        XCTAssertEqual(try Data(contentsOf: fixture), Self.waveFixture())
    }

    private static func waveFixture() -> Data {
        let samples = (0..<8_000).map { index in Int16((sin(Double(index) * 2 * .pi * 440 / 8_000) * 4_000).rounded()) }
        var data = Data()
        func text(_ string: String) { data.append(Data(string.utf8)) }
        func word(_ value: UInt16) { var little = value.littleEndian; withUnsafeBytes(of: &little) { data.append(contentsOf: $0) } }
        func integer(_ value: UInt32) { var little = value.littleEndian; withUnsafeBytes(of: &little) { data.append(contentsOf: $0) } }
        text("RIFF"); integer(UInt32(36 + samples.count * 2)); text("WAVEfmt "); integer(16)
        word(1); word(1); integer(8_000); integer(16_000); word(2); word(16)
        text("data"); integer(UInt32(samples.count * 2))
        for sample in samples { word(UInt16(bitPattern: sample)) }
        return data
    }
}

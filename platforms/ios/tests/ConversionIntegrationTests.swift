import AVFoundation
import AVKit
import CryptoKit
import CoreMedia
import CoreVideo
import SQLite3
import UIKit
import XCTest
import VelmuxUI
@testable import Velmux

/// Uses the application's existing engine and real native providers. Bundle
/// fixtures exercise owned-file bookmarks; external Files grants need UI tests.
final class ConversionIntegrationTests: XCTestCase {
    private var ownedJobs: [String] = []
    private var ownedOutputs: [URL] = []

    @MainActor
    private func applicationSession() throws -> FoundationSession {
        try XCTUnwrap((UIApplication.shared.delegate as? AppDelegate)?.session)
    }

    private func fixture(_ name: String) throws -> URL {
        try XCTUnwrap(Bundle(for: Self.self).url(
            forResource: name, withExtension: nil, subdirectory: "media-fixtures"
        ), "Run scripts/ci/prepare-media-fixtures.sh before building the test bundle")
    }

    private func onWorker<T>(_ action: @escaping () throws -> T) async throws -> T {
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<T, Error>) in
            DispatchQueue.global(qos: .userInitiated).async {
                continuation.resume(with: Result { try action() })
            }
        }
    }

    private func digest(_ url: URL) throws -> String {
        SHA256.hash(data: try Data(contentsOf: url)).map { String(format: "%02x", $0) }.joined()
    }

    private func waitForTerminal(_ media: MediaSession, id: String) throws -> JobSnapshot {
        let deadline = ProcessInfo.processInfo.systemUptime + 90
        while ProcessInfo.processInfo.systemUptime < deadline {
            let job = try XCTUnwrap(media.jobsNative().first { $0.id == id })
            if ["completed", "failed", "canceled"].contains(job.state) { return job }
            Thread.sleep(forTimeInterval: 0.025)
        }
        throw NSError(domain: "Velmux.ConversionTests", code: 1, userInfo: [NSLocalizedDescriptionKey: "Timed out waiting for job \(id)"])
    }

    private func completedURL(_ job: JobSnapshot) throws -> URL {
        XCTAssertEqual(job.state, "completed", job.error ?? "Conversion did not complete")
        let url = try XCTUnwrap(URL(string: XCTUnwrap(job.outputLocator)))
        XCTAssertTrue(url.isFileURL)
        let expectedRoot = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("Velmux", isDirectory: true).standardizedFileURL.path + "/"
        XCTAssertTrue(url.standardizedFileURL.path.hasPrefix(expectedRoot))
        let bytes = try Data(contentsOf: url)
        XCTAssertGreaterThan(bytes.count, 0)
        XCTAssertEqual(Int64(bytes.count), job.outputSize?.int64Value)
        return url
    }

    /// An independent read-only SQLite connection verifies the durable row,
    /// rather than asking the Rust cache to confirm its own representation.
    private func assertStoredJob(_ job: JobSnapshot, key: String) throws {
        let path = URL(fileURLWithPath: IosServices().storageRoot()).appendingPathComponent("operations.sqlite3").path
        var database: OpaquePointer?
        XCTAssertEqual(sqlite3_open_v2(path, &database, SQLITE_OPEN_READONLY, nil), SQLITE_OK)
        let connection = try XCTUnwrap(database)
        defer { sqlite3_close(connection) }
        sqlite3_busy_timeout(connection, 2_000)
        var statement: OpaquePointer?
        let sql = "SELECT state, record, (SELECT COUNT(*) FROM jobs WHERE submission_key=?2) FROM jobs WHERE id=?1"
        XCTAssertEqual(sqlite3_prepare_v2(connection, sql, -1, &statement, nil), SQLITE_OK)
        let query = try XCTUnwrap(statement)
        defer { sqlite3_finalize(query) }
        let transient = unsafeBitCast(-1, to: sqlite3_destructor_type.self)
        XCTAssertEqual(sqlite3_bind_text(query, 1, job.id, -1, transient), SQLITE_OK)
        XCTAssertEqual(sqlite3_bind_text(query, 2, key, -1, transient), SQLITE_OK)
        XCTAssertEqual(sqlite3_step(query), SQLITE_ROW)
        XCTAssertEqual(String(cString: try XCTUnwrap(sqlite3_column_text(query, 0))), job.state)
        XCTAssertEqual(sqlite3_column_int(query, 2), 1)
        let encoded = String(cString: try XCTUnwrap(sqlite3_column_text(query, 1)))
        let record = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(encoded.utf8)) as? [String: Any])
        XCTAssertEqual(record["id"] as? String, job.id)
        if job.state == "completed" {
            XCTAssertEqual(record["verified"] as? Bool, true)
            let output = try XCTUnwrap(record["output"] as? [String: Any])
            XCTAssertEqual(output["locator"] as? String, job.outputLocator)
        }
    }

    private struct VideoProof {
        let frames: Int
        let width: Int
        let height: Int
        let duration: Double
        let subtype: FourCharCode
    }

    private func decodeVideo(_ url: URL) throws -> VideoProof {
        let asset = AVURLAsset(url: url)
        let track = try XCTUnwrap(asset.tracks(withMediaType: .video).first)
        let descriptions = track.formatDescriptions as! [CMFormatDescription]
        let description = try XCTUnwrap(descriptions.first)
        let subtype = CMFormatDescriptionGetMediaSubType(description)
        let dimensions = CMVideoFormatDescriptionGetDimensions(description)
        let reader = try AVAssetReader(asset: asset)
        let output = AVAssetReaderTrackOutput(track: track, outputSettings: [
            kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA
        ])
        output.alwaysCopiesSampleData = false
        XCTAssertTrue(reader.canAdd(output))
        reader.add(output)
        XCTAssertTrue(reader.startReading(), reader.error?.localizedDescription ?? "AVAssetReader could not start")
        var frames = 0
        var previous = -Double.infinity
        while let sample = output.copyNextSampleBuffer() {
            XCTAssertNotNil(CMSampleBufferGetImageBuffer(sample))
            let timestamp = CMSampleBufferGetPresentationTimeStamp(sample).seconds
            XCTAssertTrue(timestamp.isFinite)
            XCTAssertGreaterThanOrEqual(timestamp, previous)
            previous = timestamp
            frames += 1
        }
        XCTAssertEqual(reader.status, .completed, reader.error?.localizedDescription ?? "Video decoding did not finish")
        return VideoProof(frames: frames, width: Int(dimensions.width), height: Int(dimensions.height), duration: asset.duration.seconds, subtype: subtype)
    }

    private struct AudioProof {
        let frames: Int
        let sampleRate: Double
        let channels: Int
        let leftHz: Double
        let rightHz: Double
        let rms: Double
        let duration: Double
        let subtype: FourCharCode
    }

    private func decodeAudio(_ url: URL) throws -> AudioProof {
        let asset = AVURLAsset(url: url)
        let track = try XCTUnwrap(asset.tracks(withMediaType: .audio).first)
        let descriptions = track.formatDescriptions as! [CMAudioFormatDescription]
        let description = try XCTUnwrap(descriptions.first)
        let format = try XCTUnwrap(CMAudioFormatDescriptionGetStreamBasicDescription(description)).pointee
        let reader = try AVAssetReader(asset: asset)
        let output = AVAssetReaderTrackOutput(track: track, outputSettings: [
            AVFormatIDKey: kAudioFormatLinearPCM,
            AVLinearPCMBitDepthKey: 16,
            AVLinearPCMIsFloatKey: false,
            AVLinearPCMIsBigEndianKey: false,
            AVLinearPCMIsNonInterleaved: false,
        ])
        output.alwaysCopiesSampleData = false
        XCTAssertTrue(reader.canAdd(output))
        reader.add(output)
        XCTAssertTrue(reader.startReading(), reader.error?.localizedDescription ?? "Audio decoding could not start")
        var frames = 0
        var pcm = Data()
        while let sample = output.copyNextSampleBuffer() {
            frames += CMSampleBufferGetNumSamples(sample)
            let block = try XCTUnwrap(CMSampleBufferGetDataBuffer(sample))
            let count = CMBlockBufferGetDataLength(block)
            var chunk = Data(count: count)
            let status = chunk.withUnsafeMutableBytes { bytes in
                CMBlockBufferCopyDataBytes(block, atOffset: 0, dataLength: count, destination: bytes.baseAddress!)
            }
            XCTAssertEqual(status, kCMBlockBufferNoErr)
            pcm.append(chunk)
        }
        XCTAssertEqual(reader.status, .completed, reader.error?.localizedDescription ?? "Audio decoding did not finish")
        let channels = Int(format.mChannelsPerFrame)
        XCTAssertEqual(channels, 2)
        guard channels == 2, frames > 0, pcm.count >= frames * channels * 2 else {
            throw NSError(domain: "Velmux.ConversionTests", code: 2, userInfo: [NSLocalizedDescriptionKey: "Missing decoded stereo PCM"])
        }
        var previous = [Int16](repeating: 0, count: channels)
        var crossings = [Int](repeating: 0, count: channels)
        var energy = 0.0
        pcm.withUnsafeBytes { bytes in
            for frame in 0..<frames {
                for channel in 0..<channels {
                    let sample = Int16(littleEndian: bytes.loadUnaligned(fromByteOffset: (frame * channels + channel) * 2, as: Int16.self))
                    if previous[channel] <= 0 && sample > 0 { crossings[channel] += 1 }
                    previous[channel] = sample
                    let value = Double(sample) / 32768
                    energy += value * value
                }
            }
        }
        let seconds = Double(frames) / format.mSampleRate
        return AudioProof(frames: frames, sampleRate: format.mSampleRate, channels: channels,
                          leftHz: Double(crossings[0]) / seconds, rightHz: Double(crossings[1]) / seconds,
                          rms: sqrt(energy / Double(frames * channels)), duration: asset.duration.seconds,
                          subtype: CMFormatDescriptionGetMediaSubType(description))
    }

    @MainActor
    func testVideoAndAudioConversionsPublishRealDecodedMediaAndDurableJobs() async throws {
        let session = try applicationSession()
        let media = session.media
        let video = try fixture("video.mp4")
        let audio = try fixture("stereo.wav")
        let videoHash = try digest(video)
        let audioHash = try digest(audio)
        let identifiers = UUID().uuidString.lowercased()
        let videoKey = "ios-video-" + identifiers
        let audioKey = "ios-audio-" + identifiers
        let result = try await onWorker { () -> (JobSnapshot, JobSnapshot) in
            try media.beginNative()
            let selectedVideo = try IosMediaServices.selectedSource(url: video)
            let inspectedVideo = try media.inspectNative(locator: selectedVideo.locator, name: selectedVideo.displayName, video: true)
            defer { try? media.releaseNative(id: inspectedVideo.id) }
            let selectedAudio = try IosMediaServices.selectedSource(url: audio)
            let inspectedAudio = try media.inspectNative(locator: selectedAudio.locator, name: selectedAudio.displayName, video: false)
            defer { try? media.releaseNative(id: inspectedAudio.id) }
            let videoConfig = try media.encodingConfigurationNative(id: inspectedVideo.id, video: true, fileName: videoKey)
            let audioConfig = try media.encodingConfigurationNative(id: inspectedAudio.id, video: false, fileName: audioKey)
            try media.validateNative(id: inspectedVideo.id, configuration: videoConfig)
            try media.validateNative(id: inspectedAudio.id, configuration: audioConfig)
            let first = try media.submitNative(id: inspectedVideo.id, key: videoKey, configuration: videoConfig)
            let repeated = try media.submitNative(id: inspectedVideo.id, key: videoKey, configuration: videoConfig)
            XCTAssertEqual(first.id, repeated.id)
            let second = try media.submitNative(id: inspectedAudio.id, key: audioKey, configuration: audioConfig)
            return (try self.waitForTerminal(media, id: first.id), try self.waitForTerminal(media, id: second.id))
        }
        ownedJobs += [result.0.id, result.1.id]
        let videoURL = try completedURL(result.0)
        let audioURL = try completedURL(result.1)
        ownedOutputs += [videoURL, audioURL]
        try await onWorker {
            let picture = try self.decodeVideo(videoURL)
            XCTAssertEqual(picture.subtype, kCMVideoCodecType_H264)
            XCTAssertEqual(picture.width, 320)
            XCTAssertEqual(picture.height, 180)
            XCTAssertEqual(picture.frames, 48)
            XCTAssertEqual(picture.duration, 2, accuracy: 0.15)
            let sound = try self.decodeAudio(audioURL)
            XCTAssertEqual(sound.subtype, kAudioFormatMPEG4AAC)
            XCTAssertEqual(sound.sampleRate, 48_000)
            XCTAssertEqual(sound.channels, 2)
            XCTAssertLessThanOrEqual(abs(sound.frames - 96_000), 2_048)
            XCTAssertEqual(sound.leftHz, 440, accuracy: 5)
            XCTAssertEqual(sound.rightHz, 660, accuracy: 5)
            XCTAssertGreaterThan(sound.rms, 0.05)
            XCTAssertLessThan(sound.rms, 0.25)
            XCTAssertEqual(sound.duration, 2, accuracy: 0.1)
            try self.assertStoredJob(result.0, key: videoKey)
            try self.assertStoredJob(result.1, key: audioKey)
            let proof = XCTAttachment(string: "Video: H264 \(picture.width)x\(picture.height), \(picture.frames) decoded frames, \(picture.duration)s. Audio: AAC \(sound.sampleRate)Hz, \(sound.frames) decoded stereo frames, \(sound.leftHz)/\(sound.rightHz)Hz, RMS \(sound.rms). Durable SQLite rows and duplicate key count verified.")
            proof.name = "Independent native media verification"
            proof.lifetime = .keepAlways
            self.add(proof)
        }
        XCTAssertEqual(try digest(video), videoHash)
        XCTAssertEqual(try digest(audio), audioHash)
        try await onWorker {
            try media.dismissNative(id: result.0.id)
            try media.dismissNative(id: result.1.id)
            let retained = try media.jobsNative().map(\.id)
            XCTAssertFalse(retained.contains(result.0.id))
            XCTAssertFalse(retained.contains(result.1.id))
        }
        XCTAssertTrue(FileManager.default.fileExists(atPath: videoURL.path))
        XCTAssertTrue(FileManager.default.fileExists(atPath: audioURL.path))
        XCTAssertTrue(try session.inspect().encoderCount > 0, "The borrowed application session must remain live")
    }

    @MainActor
    func testRequestCancellationErrorsAndAcceptedJobCancellationRemainCatchable() async throws {
        let media = try applicationSession().media
        let sourceURL = try fixture("video-large.mp4")
        let sourceHash = try digest(sourceURL)
        let invalidURL = try fixture("invalid.bin")
        let key = "ios-cancellation-" + UUID().uuidString.lowercased()
        let terminal = try await onWorker { () -> JobSnapshot in
            try media.beginNative()
            let before = try media.jobsNative().map(\.id).sorted()
            let canceled = try media.newRequest()
            canceled.cancel()
            defer { canceled.close() }
            XCTAssertThrowsError(try media.inspectWithRequestNative(locator: sourceURL.absoluteString, name: "video-large.mp4", video: true, request: canceled))
            XCTAssertThrowsError(try media.inspectNative(locator: invalidURL.absoluteString, name: "invalid.bin", video: true))
            XCTAssertEqual(try media.jobsNative().map(\.id).sorted(), before)
            let selected = try IosMediaServices.selectedSource(url: sourceURL)
            let source = try media.inspectNative(locator: selected.locator, name: selected.displayName, video: true)
            defer { try? media.releaseNative(id: source.id) }
            let config = try media.encodingConfigurationNative(id: source.id, video: true, fileName: key)
            let job = try media.submitNative(id: source.id, key: key, configuration: config)
            try media.cancelNative(id: job.id)
            try media.cancelNative(id: job.id)
            return try self.waitForTerminal(media, id: job.id)
        }
        ownedJobs.append(terminal.id)
        XCTAssertTrue(["canceled", "completed"].contains(terminal.state), terminal.error ?? terminal.state)
        if terminal.state == "canceled" {
            XCTAssertNil(terminal.outputLocator)
        } else {
            // Completion can legitimately win a cancellation race on fast runners.
            let url = try completedURL(terminal)
            ownedOutputs.append(url)
            let frames = try await onWorker { try self.decodeVideo(url).frames }
            XCTAssertGreaterThan(frames, 0)
        }
        try await onWorker {
            try self.assertStoredJob(terminal, key: key)
            try media.dismissNative(id: terminal.id)
        }
        if let output = terminal.outputLocator.flatMap(URL.init(string:)) {
            XCTAssertTrue(FileManager.default.fileExists(atPath: output.path))
        }
        XCTAssertEqual(try digest(sourceURL), sourceHash)
        let evidence = XCTAttachment(string: "Accepted job cancellation race settled as \(terminal.state); repeated cancellation returned normally; original hash unchanged.")
        evidence.name = "Cancellation terminal outcome"
        evidence.lifetime = .keepAlways
        add(evidence)
    }

    @MainActor
    func testLazySeekedPreviewDecodesAndNativePlayerBecomesReady() async throws {
        let host = try XCTUnwrap(UIApplication.shared.delegate as? AppDelegate)
        let media = try XCTUnwrap(host.session).media
        let sourceURL = try fixture("video.mp4")
        let original = try digest(sourceURL)
        let source = try await onWorker {
            try media.inspectNative(locator: sourceURL.absoluteString, name: "video.mp4", video: true)
        }
        defer { try? media.releaseNative(id: source.id) }
        let jobsBefore = try await onWorker { try media.jobsNative().map(\.id).sorted() }
        let clip = try await onWorker { try media.previewNative(id: source.id, startUs: 500_000) }
        XCTAssertEqual(clip.startUs, 500_000)
        XCTAssertGreaterThan(clip.durationUs, 0)
        XCTAssertLessThanOrEqual(clip.durationUs, 1_750_000)
        let url = try XCTUnwrap(URL(string: clip.locator))
        let picture = try await onWorker { try self.decodeVideo(url) }
        XCTAssertEqual(picture.subtype, kCMVideoCodecType_H264)
        XCTAssertLessThanOrEqual(max(picture.width, picture.height), 640)
        XCTAssertGreaterThan(picture.frames, 0)
        XCTAssertLessThanOrEqual(picture.duration, 1.75)
        let jobsAfter = try await onWorker { try media.jobsNative().map(\.id).sorted() }
        XCTAssertEqual(jobsAfter, jobsBefore)
        let actions = try XCTUnwrap(host.mediaActions)
        let controller = actions.createPreview(locator: clip.locator) {
            XCTFail("The encoded fallback preview must be playable")
            return
        }
        defer { actions.releasePreview(controller: controller) }
        controller.loadViewIfNeeded()
        let playerController = try XCTUnwrap(controller.children.first as? AVPlayerViewController)
        let player = try XCTUnwrap(playerController.player)
        let item = try XCTUnwrap(player.currentItem)
        let ready = expectation(description: "Encoded preview is ready in AVPlayer")
        var didBecomeReady = false
        let observation = item.observe(\.status, options: [.initial, .new]) { item, _ in
            if item.status == .readyToPlay && !didBecomeReady { didBecomeReady = true; ready.fulfill() }
        }
        await fulfillment(of: [ready], timeout: 15)
        observation.invalidate()
        XCTAssertEqual(item.status, .readyToPlay)
        let sought = expectation(description: "Encoded preview seeks")
        player.seek(to: CMTime(seconds: 0.25, preferredTimescale: 48_000), toleranceBefore: .zero, toleranceAfter: .zero) { finished in
            XCTAssertTrue(finished)
            sought.fulfill()
        }
        await fulfillment(of: [sought], timeout: 10)
        XCTAssertEqual(player.currentTime().seconds, 0.25, accuracy: 0.05)
        XCTAssertEqual(try digest(sourceURL), original)
    }

    @MainActor
    func testNativeDocumentPickerPresentationAndCancellationAreSingleShot() async throws {
        let host = try XCTUnwrap(UIApplication.shared.delegate as? AppDelegate)
        let actions = try XCTUnwrap(host.mediaActions)
        let root = try XCTUnwrap(host.window?.rootViewController)
        let canceled = expectation(description: "Native picker cancellation callback")
        var calls = 0
        actions.selectSource(video: true) { selected, error in
            calls += 1
            XCTAssertNil(selected)
            XCTAssertNil(error, "Picker cancellation is not an error")
            canceled.fulfill()
            return
        }
        var picker: UIDocumentPickerViewController?
        for _ in 0..<100 {
            picker = root.presentedViewController as? UIDocumentPickerViewController
            if picker != nil { break }
            try await Task.sleep(nanoseconds: 20_000_000)
        }
        let presented = try XCTUnwrap(picker)
        XCTAssertFalse(presented.allowsMultipleSelection)
        // This verifies UIKit presentation and delegate cancellation. It does
        // not represent an automated selection from an external Files provider.
        actions.documentPickerWasCancelled(presented)
        actions.documentPickerWasCancelled(presented)
        presented.dismiss(animated: false)
        await fulfillment(of: [canceled], timeout: 5)
        XCTAssertEqual(calls, 1)
    }

    override func tearDownWithError() throws {
        // These files were created by this test, under uniquely owned names.
        // Application dismissal/cache clearing never removes published output.
        for url in ownedOutputs { try? FileManager.default.removeItem(at: url) }
        ownedOutputs.removeAll()
        ownedJobs.removeAll()
    }
}

import AVFoundation
import AVKit
import CryptoKit
import CoreMedia
import CoreVideo
import VideoToolbox
import SQLite3
import UIKit
import UniformTypeIdentifiers
import XCTest
@testable import Velmux

/// Uses the application's existing engine and real native providers. Bundle
/// fixtures exercise owned-file bookmarks; external Files grants need UI tests.
final class ConversionIntegrationTests: XCTestCase {
    private var ownedInspections = Set<String>()
    private var ownedJobs: [String: String] = [:]
    private var ownedOutputs = Set<URL>()
    private var originalHashes: [URL: String] = [:]

    @MainActor
    private func applicationSession() async throws -> AppSession {
        let session = AppSession.shared
        await session.start()
        let deadline = ProcessInfo.processInfo.systemUptime + 45
        while !session.ready, session.phase != .failed, ProcessInfo.processInfo.systemUptime < deadline {
            try await Task.sleep(nanoseconds: 20_000_000)
        }
        XCTAssertTrue(session.ready)
        return session
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

    private func nativeVideoRequest(
        codec: VideoCodec = .h264, width: UInt32 = 320, height: UInt32 = 180,
        numerator: Int32 = 24, denominator: Int32 = 1,
        bitRate: UInt64 = 2_000_000, pixelFormat: String = "nv12", bitDepth: UInt32 = 8,
        hdr: Bool = false, transfer: String = "bt709"
    ) -> NativeVideoRequest {
        NativeVideoRequest(
            codec: codec, width: width, height: height,
            frameRate: Rational(numerator: numerator, denominator: denominator),
            bitRate: bitRate, pixelFormat: pixelFormat, bitDepth: bitDepth, hdr: hdr,
            colorSpace: "bt709", colorRange: "tv", colorPrimaries: "bt709", colorTransfer: transfer
        )
    }

    private struct NativePreparationProof {
        let stage: String
        let status: OSStatus
        let identifier: String?
        let hardware: Bool?
        var prepared: Bool { stage == "prepared" && status == noErr }
    }

    /// This test calls VideoToolbox independently to verify a returned identity
    /// or provide a concrete OS failure when the provider has no candidate.
    private func prepareBaselineNativeEncoder(identifier: String?) -> NativePreparationProof {
        var specification: [String: Any] = [:]
        if let identifier { specification[kVTVideoEncoderSpecification_EncoderID as String] = identifier }
        if #available(iOS 17.4, *) {
            specification[kVTVideoEncoderSpecification_EnableHardwareAcceleratedVideoEncoder as String] = true
        }
        let attributes: [String: Any] = [
            kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
            kCVPixelBufferWidthKey as String: 320, kCVPixelBufferHeightKey as String: 180,
        ]
        var created: VTCompressionSession?
        let createStatus = VTCompressionSessionCreate(
            allocator: kCFAllocatorDefault, width: 320, height: 180, codecType: kCMVideoCodecType_H264,
            encoderSpecification: specification as CFDictionary, imageBufferAttributes: attributes as CFDictionary,
            compressedDataAllocator: nil, outputCallback: nil, refcon: nil, compressionSessionOut: &created
        )
        guard let session = created else {
            return NativePreparationProof(stage: "create", status: createStatus, identifier: nil, hardware: nil)
        }
        defer { VTCompressionSessionInvalidate(session) }
        guard createStatus == noErr else {
            return NativePreparationProof(stage: "create", status: createStatus, identifier: nil, hardware: nil)
        }
        let properties: [String: Any] = [
            kVTCompressionPropertyKey_ProfileLevel as String: kVTProfileLevel_H264_High_AutoLevel,
            kVTCompressionPropertyKey_AverageBitRate as String: 2_000_000,
            kVTCompressionPropertyKey_ExpectedFrameRate as String: 24,
            kVTCompressionPropertyKey_AllowFrameReordering as String: false,
            kVTCompressionPropertyKey_RealTime as String: false,
            kVTCompressionPropertyKey_YCbCrMatrix as String: kCVImageBufferYCbCrMatrix_ITU_R_709_2,
            kVTCompressionPropertyKey_ColorPrimaries as String: kCVImageBufferColorPrimaries_ITU_R_709_2,
            kVTCompressionPropertyKey_TransferFunction as String: kCVImageBufferTransferFunction_ITU_R_709_2,
        ]
        let configureStatus = VTSessionSetProperties(session, propertyDictionary: properties as CFDictionary)
        guard configureStatus == noErr else {
            return NativePreparationProof(stage: "configure", status: configureStatus, identifier: nil, hardware: nil)
        }
        let prepareStatus = VTCompressionSessionPrepareToEncodeFrames(session)
        guard prepareStatus == noErr else {
            return NativePreparationProof(stage: "prepare", status: prepareStatus, identifier: nil, hardware: nil)
        }
        var retainedID: Unmanaged<CFTypeRef>?
        let identityStatus = VTSessionCopyProperty(session, key: kVTCompressionPropertyKey_EncoderID,
                                                   allocator: kCFAllocatorDefault, valueOut: &retainedID)
        let encoderID = retainedID?.takeRetainedValue() as? String
        guard identityStatus == noErr, let encoderID, !encoderID.isEmpty else {
            return NativePreparationProof(stage: "encoder identity", status: identityStatus, identifier: nil, hardware: nil)
        }
        var hardware: Bool?
        if #available(iOS 17.4, *) {
            var retained: Unmanaged<CFTypeRef>?
            let status = VTSessionCopyProperty(session, key: kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder,
                                               allocator: kCFAllocatorDefault, valueOut: &retained)
            if let value = retained?.takeRetainedValue(), status == noErr,
               CFGetTypeID(value) == CFBooleanGetTypeID() { hardware = value as? Bool }
        }
        return NativePreparationProof(stage: "prepared", status: noErr, identifier: encoderID, hardware: hardware)
    }

    @MainActor
    func testNativeCapabilitiesPrepareRealSessionsWithoutClaimingEncodedHardwareOutput() async throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("velmux-capability-query-" + UUID().uuidString, isDirectory: true)
        let evidence = try await onWorker { () -> [String] in
            let services = IosMediaServices(
                operationalRoot: root.appendingPathComponent("support"),
                cacheDirectory: root.appendingPathComponent("cache"),
                outputDirectory: root.appendingPathComponent("output")
            )
            XCTAssertFalse(FileManager.default.fileExists(atPath: root.path))
            let invalid = [
                self.nativeVideoRequest(width: 0), self.nativeVideoRequest(height: UInt32.max),
                self.nativeVideoRequest(numerator: 0), self.nativeVideoRequest(denominator: 0),
                self.nativeVideoRequest(denominator: -1), self.nativeVideoRequest(bitRate: 0),
                self.nativeVideoRequest(pixelFormat: "p010le", bitDepth: 10),
                self.nativeVideoRequest(hdr: true), self.nativeVideoRequest(transfer: "smpte2084"),
                self.nativeVideoRequest(codec: .av1),
            ]
            for request in invalid {
                XCTAssertTrue(try services.nativeVideoCandidates(request: request).isEmpty,
                              "The initial native route must reject unsupported or invalid representations")
            }
            let candidates = try services.nativeVideoCandidates(request: self.nativeVideoRequest())
            XCTAssertLessThanOrEqual(candidates.count, 1, "This probe must prepare at most one session per request")
            var lines = [
                "Runtime: " + ProcessInfo.processInfo.operatingSystemVersionString,
                "Baseline H264320x180,24fps,2Mbps,NV12,SDR BT709 limited.",
                "Session creation/preparation only. No media frame was encoded; no physical-device or hardware-output claim follows.",
            ]
            if candidates.isEmpty {
                let actual = self.prepareBaselineNativeEncoder(identifier: nil)
                XCTAssertFalse(actual.prepared, "The provider omitted a real baseline session that the runtime OS can prepare")
                lines.append("No native candidate: independent VideoToolbox stage=\(actual.stage), OSStatus=\(actual.status), encoderID=\(actual.identifier ?? "unavailable"). Software FFmpeg processing remains independent.")
            }
            for candidate in candidates {
                XCTAssertFalse(candidate.identifier.isEmpty)
                XCTAssertLessThanOrEqual(candidate.identifier.utf8.count, 256)
                XCTAssertTrue(candidate.sessionPrepared, "The provider must actually prepare its reported session")
                let actual = self.prepareBaselineNativeEncoder(identifier: candidate.identifier)
                XCTAssertTrue(actual.prepared, "The reported OS encoder cannot prepare the exact baseline: \(actual.stage), OSStatus=\(actual.status)")
                XCTAssertEqual(candidate.identifier, actual.identifier)
                XCTAssertEqual(candidate.osReportsHardware, actual.hardware,
                               "Classification describes this prepared OS session, not encoded output")
                if #available(iOS 17.4, *) {} else { XCTAssertNil(candidate.osReportsHardware) }
                lines.append("\(candidate.identifier): sessionPrepared=\(candidate.sessionPrepared), osReportsHardware=\(String(describing: candidate.osReportsHardware)); independently prepared matching OS encoder identity.")
            }
            XCTAssertFalse(FileManager.default.fileExists(atPath: root.path), "Capability inspection must not create operational, cache or output files")
            return lines
        }
        let attachment = XCTAttachment(string: evidence.joined(separator: "\n"))
        attachment.name = "Native encoder capability preparation evidence"
        attachment.lifetime = .keepAlways
        add(attachment)
    }

    private func digest(_ url: URL) throws -> String {
        SHA256.hash(data: try Data(contentsOf: url)).map { String(format: "%02x", $0) }.joined()
    }

    @MainActor
    private func waitForTerminal(_ session: AppSession, id: String) async throws -> JobRecord {
        let deadline = ProcessInfo.processInfo.systemUptime + 90
        while ProcessInfo.processInfo.systemUptime < deadline {
            if let job = session.jobs.first(where: { $0.id == id }),
               [.completed, .failed, .canceled, .targetNotMet].contains(job.state) { return job }
            try await Task.sleep(nanoseconds: 25_000_000)
        }
        throw NSError(domain: "Velmux.ConversionTests", code: 1, userInfo: [NSLocalizedDescriptionKey: "Timed out waiting for job \(id)"])
    }

    private func completedURL(_ job: JobRecord) throws -> URL {
        XCTAssertEqual(job.state, .completed, job.error ?? "Conversion did not complete")
        let url = try XCTUnwrap(URL(string: XCTUnwrap(job.output?.locator)))
        XCTAssertTrue(url.isFileURL)
        let expectedRoot = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("Velmux", isDirectory: true).standardizedFileURL.path + "/"
        XCTAssertTrue(url.standardizedFileURL.path.hasPrefix(expectedRoot))
        guard url.isFileURL, url.standardizedFileURL.path.hasPrefix(expectedRoot),
              ownedJobs[job.id] == job.fileName, url.lastPathComponent == job.fileName else {
            throw NSError(domain: "Velmux.ConversionTests", code: 3, userInfo: [NSLocalizedDescriptionKey: "The published result is outside this test's explicit output ownership"])
        }
        let bytes = try Data(contentsOf: url)
        XCTAssertGreaterThan(bytes.count, 0)
        XCTAssertEqual(UInt64(bytes.count), job.output?.size)
        return url
    }

    /// An independent read-only SQLite connection verifies the durable row,
    /// rather than asking the Rust cache to confirm its own representation.
    private func assertStoredJob(_ job: JobRecord, key: String) throws {
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
        XCTAssertEqual(String(cString: try XCTUnwrap(sqlite3_column_text(query, 0))), String(describing: job.state))
        XCTAssertEqual(sqlite3_column_int(query, 2), 1)
        let encoded = String(cString: try XCTUnwrap(sqlite3_column_text(query, 1)))
        let record = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(encoded.utf8)) as? [String: Any])
        XCTAssertEqual(record["id"] as? String, job.id)
        if job.state == .completed {
            XCTAssertEqual(record["verified"] as? Bool, true)
            let output = try XCTUnwrap(record["output"] as? [String: Any])
            XCTAssertEqual(output["locator"] as? String, job.output?.locator)
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
        let session = try await applicationSession()
        let video = try fixture("video.mp4"), audio = try fixture("stereo.wav")
        let videoHash = try digest(video), audioHash = try digest(audio)
        originalHashes[video] = videoHash; originalHashes[audio] = audioHash
        let videoKey = "ios-video-" + UUID().uuidString.lowercased()
        let audioKey = "ios-audio-" + UUID().uuidString.lowercased()
        let inspectedVideo = try await session.inspect(IosMediaServices.selectedSource(url: video), operation: .videoConverter)
        ownedInspections.insert(inspectedVideo.id)
        let inspectedAudio = try await session.inspect(IosMediaServices.selectedSource(url: audio), operation: .audioConverter)
        ownedInspections.insert(inspectedAudio.id)
        XCTAssertFalse(inspectedVideo.info.staged)
        XCTAssertEqual(inspectedVideo.info.streams.filter { $0.kind == .video }.count, 1)
        XCTAssertEqual(inspectedAudio.info.streams.first(where: { $0.kind == .audio })?.audio?.channels, 2)
        var videoConfig = try await session.defaults(sourceId: inspectedVideo.id, operation: .videoConverter)
        videoConfig.format = .mp4; videoConfig.videoCodec = .h264; videoConfig.fileName = videoKey
        var audioConfig = try await session.defaults(sourceId: inspectedAudio.id, operation: .audioConverter)
        audioConfig.format = .m4a; audioConfig.audioCodec = .aac; audioConfig.fileName = audioKey
        XCTAssertTrue(try session.catalog(sourceId: inspectedVideo.id, operation: .videoConverter).contains { $0.format == .mp4 })
        _ = try session.choices(sourceId: inspectedVideo.id, configuration: videoConfig)
        try await session.validate(sourceId: inspectedVideo.id, configuration: videoConfig)
        try await session.validate(sourceId: inspectedAudio.id, configuration: audioConfig)
        let first = try await session.submit(sourceId: inspectedVideo.id, key: videoKey, configuration: videoConfig)
        ownedJobs[first.id] = videoKey + ".mp4"
        let repeated = try await session.submit(sourceId: inspectedVideo.id, key: videoKey, configuration: videoConfig)
        ownedJobs[repeated.id] = videoKey + ".mp4"
        XCTAssertEqual(first.id, repeated.id)
        let second = try await session.submit(sourceId: inspectedAudio.id, key: audioKey, configuration: audioConfig)
        ownedJobs[second.id] = audioKey + ".m4a"
        // Accepted jobs retain their own source grants after workspace disposal.
        try await session.releaseInspection(id: inspectedVideo.id)
        ownedInspections.remove(inspectedVideo.id)
        try await session.releaseInspection(id: inspectedAudio.id)
        ownedInspections.remove(inspectedAudio.id)
        videoConfig.fileName = "unaccepted-edit"; audioConfig.audioCodec = .flac
        let videoJob = try await waitForTerminal(session, id: first.id)
        let audioJob = try await waitForTerminal(session, id: second.id)
        let videoURL = try completedURL(videoJob)
        ownedOutputs.insert(videoURL)
        let audioURL = try completedURL(audioJob)
        ownedOutputs.insert(audioURL)
        try await onWorker {
            let picture = try self.decodeVideo(videoURL)
            XCTAssertEqual(picture.subtype, kCMVideoCodecType_H264)
            XCTAssertEqual(picture.width, 320); XCTAssertEqual(picture.height, 180)
            XCTAssertEqual(picture.frames, 48); XCTAssertEqual(picture.duration, 2, accuracy: 0.15)
            let sound = try self.decodeAudio(audioURL)
            XCTAssertEqual(sound.subtype, kAudioFormatMPEG4AAC)
            XCTAssertEqual(sound.sampleRate, 48_000); XCTAssertEqual(sound.channels, 2)
            XCTAssertLessThanOrEqual(abs(sound.frames - 96_000), 2_048)
            XCTAssertEqual(sound.leftHz, 440, accuracy: 5); XCTAssertEqual(sound.rightHz, 660, accuracy: 5)
            XCTAssertGreaterThan(sound.rms, 0.05); XCTAssertLessThan(sound.rms, 0.25)
            XCTAssertEqual(sound.duration, 2, accuracy: 0.1)
            try self.assertStoredJob(videoJob, key: videoKey)
            try self.assertStoredJob(audioJob, key: audioKey)
            let proof = XCTAttachment(string: "Direct Swift application session: H264 \(picture.width)x\(picture.height), \(picture.frames) independently decoded frames; AAC \(sound.frames) stereo samples, \(sound.leftHz)/\(sound.rightHz)Hz. SQLite completion and duplicate submission verified.")
            proof.name = "Independent native conversion verification"; proof.lifetime = .keepAlways; self.add(proof)
        }
        XCTAssertEqual(try digest(video), videoHash); XCTAssertEqual(try digest(audio), audioHash)
        try await session.dismissJob(id: videoJob.id)
        ownedJobs.removeValue(forKey: videoJob.id)
        try await session.dismissJob(id: audioJob.id)
        ownedJobs.removeValue(forKey: audioJob.id)
        XCTAssertTrue(FileManager.default.fileExists(atPath: videoURL.path))
        XCTAssertTrue(FileManager.default.fileExists(atPath: audioURL.path))
        XCTAssertTrue(session.ready)
    }

    @MainActor
    func testAudioGainAndFadesReachRealNativeDecodedSamples() async throws {
        let session = try await applicationSession()
        let original = try fixture("stereo.wav")
        originalHashes[original] = try digest(original)
        let inspected = try await session.inspect(IosMediaServices.selectedSource(url: original), operation: .audioConverter)
        ownedInspections.insert(inspected.id)
        var configuration = try await session.defaults(sourceId: inspected.id, operation: .audioConverter)
        configuration.format = .wav; configuration.audioCodec = .pcm
        configuration.fileName = "ios-adjustments-" + UUID().uuidString.lowercased()
        configuration.audioAdjustments = AudioAdjustments(gainCentidb: -602, fadeInUs: 500_000, fadeOutUs: 500_000)
        let choices = try session.choices(sourceId: inspected.id, configuration: configuration)
        XCTAssertEqual(choices.audioAdjustmentLimits?.minimumGainCentidb, -6000)
        var invalid = configuration
        invalid.audioAdjustments = AudioAdjustments(gainCentidb: 2401, fadeInUs: 0, fadeOutUs: 0)
        do { try await session.validate(sourceId: inspected.id, configuration: invalid); XCTFail("Rust accepted invalid audio gain") }
        catch EngineError.Configuration {}
        let accepted = try await session.submit(sourceId: inspected.id, key: configuration.fileName, configuration: configuration)
        ownedJobs[accepted.id] = configuration.fileName + ".wav"
        let completed = try await waitForTerminal(session, id: accepted.id)
        let output = try completedURL(completed)
        ownedOutputs.insert(output)
        let proof = try await onWorker { try self.decodeAudio(output) }
        XCTAssertEqual(proof.frames, 96_000)
        XCTAssertEqual(proof.sampleRate, 48_000)
        XCTAssertEqual(proof.channels, 2)
        XCTAssertEqual(proof.leftHz, 440, accuracy: 1)
        XCTAssertEqual(proof.rightHz, 660, accuracy: 1)
        XCTAssertEqual(proof.duration, 2, accuracy: 0.01)
        let expectedRms = 0.15 / sqrt(2) * pow(10, -6.02 / 20) * sqrt(2.0 / 3)
        XCTAssertEqual(proof.rms, expectedRms, accuracy: 0.001,
                       "Real sample energy must reflect gain and both linear fade envelopes")
    }

    @MainActor
    func testCoreValidationAndCancellationKeepOriginalsAndOtherRequestsResponsive() async throws {
        let session = try await applicationSession()
        let sourceURL = try fixture("long-preview.mp4"), invalidURL = try fixture("invalid.bin")
        let sourceHash = try digest(sourceURL)
        originalHashes[sourceURL] = sourceHash
        originalHashes[invalidURL] = try digest(invalidURL)
        do {
            let unexpected = try await session.inspect(IosSelectedSource(locator: invalidURL.absoluteString, displayName: "invalid.bin"), operation: .videoConverter)
            ownedInspections.insert(unexpected.id)
            XCTFail("Invalid actual contents must be rejected by Rust")
        } catch EngineError.InvalidMedia {} catch EngineError.UnsupportedMedia {}
        let canceled = Task { try await session.inspect(IosSelectedSource(locator: sourceURL.absoluteString, displayName: sourceURL.lastPathComponent), operation: .videoConverter) }
        canceled.cancel()
        do {
            let unexpected = try await canceled.value
            ownedInspections.insert(unexpected.id)
            XCTFail("Canceled inspection unexpectedly succeeded")
        }
        catch EngineError.Cancelled {} catch is CancellationError {}
        let inspected = try await session.inspect(IosMediaServices.selectedSource(url: sourceURL), operation: .videoConverter)
        ownedInspections.insert(inspected.id)
        var configuration = try await session.defaults(sourceId: inspected.id, operation: .videoConverter)
        configuration.format = .mp4; configuration.videoCodec = .h264
        configuration.frameRate = .fps60
        // The small 160×90 source can finish before a progress snapshot reaches
        // the UI on fast runners. Explicit custom upscaling creates enough real
        // encoding work to test in-flight cancellation without a synthetic gate.
        configuration.videoSize = .custom
        configuration.customWidth = 1920; configuration.customHeight = 1080
        configuration.fileName = "ios-cancellation-" + UUID().uuidString.lowercased()
        var invalid = configuration; invalid.customWidth = 0; invalid.customHeight = 180; invalid.videoSize = .custom
        do { try await session.validate(sourceId: inspected.id, configuration: invalid); XCTFail("Rust accepted invalid geometry") }
        catch EngineError.Configuration {}
        let accepted = try await session.submit(sourceId: inspected.id, key: configuration.fileName, configuration: configuration)
        ownedJobs[accepted.id] = configuration.fileName + ".mp4"
        let progressDeadline = ProcessInfo.processInfo.systemUptime + 30
        var observedProgress = false
        while ProcessInfo.processInfo.systemUptime < progressDeadline {
            if let job = session.jobs.first(where: { $0.id == accepted.id }) {
                if job.state == .processing && job.processedUs > 0 { observedProgress = true; break }
                if [.completed, .failed, .canceled].contains(job.state) { break }
            }
            try await Task.sleep(nanoseconds: 10_000_000)
        }
        XCTAssertTrue(observedProgress, "The deterministic cancellation fixture must reach real processing progress")
        let began = ProcessInfo.processInfo.systemUptime
        _ = try session.choices(sourceId: inspected.id, configuration: configuration)
        try await session.refreshCache()
        XCTAssertLessThan(ProcessInfo.processInfo.systemUptime - began, 5, "Ordinary requests must remain responsive during encoding")
        try await session.cancelJob(id: accepted.id)
        try await session.cancelJob(id: accepted.id)
        let terminal = try await waitForTerminal(session, id: accepted.id)
        XCTAssertEqual(terminal.state, .canceled, terminal.error ?? "Cancellation was not acknowledged")
        XCTAssertNil(terminal.output)
        try await session.releaseInspection(id: inspected.id)
        ownedInspections.remove(inspected.id)
        try await session.clearCache()
        XCTAssertEqual(try digest(sourceURL), sourceHash)
        try await session.dismissJob(id: terminal.id)
        ownedJobs.removeValue(forKey: terminal.id)
    }

    @MainActor
    func testDecodedFallbackPreviewIsSeekableAndDoesNotCreateAJob() async throws {
        let session = try await applicationSession()
        let originalURL = try fixture("video.mp4"), original = try digest(fixture("video.mp4"))
        originalHashes[originalURL] = original
        let source = try await session.inspect(IosMediaServices.selectedSource(url: originalURL), operation: .videoConverter)
        ownedInspections.insert(source.id)
        let before = session.jobs.map(\.id).sorted()
        let clip = try await session.preparePreview(sourceId: source.id, startUs: 500_000)
        XCTAssertEqual(clip.startUs, 500_000)
        XCTAssertGreaterThan(clip.durationUs, 0); XCTAssertLessThanOrEqual(clip.durationUs, 1_750_000)
        let url = try XCTUnwrap(URL(string: clip.locator))
        let picture = try await onWorker { try self.decodeVideo(url) }
        XCTAssertEqual(picture.subtype, kCMVideoCodecType_H264)
        XCTAssertLessThanOrEqual(max(picture.width, picture.height), 640)
        XCTAssertGreaterThan(picture.frames, 0); XCTAssertLessThanOrEqual(picture.duration, 1.75)
        XCTAssertEqual(session.jobs.map(\.id).sorted(), before)
        let actions = IosMediaActions(presentationHost: { nil })
        let controller = actions.createPreview(locator: clip.locator) { XCTFail("The real encoded preview must play natively") }
        defer { actions.releasePreview(controller: controller) }
        controller.loadViewIfNeeded()
        let playback = try XCTUnwrap(controller.children.first as? AVPlayerViewController)
        let deadline = ProcessInfo.processInfo.systemUptime + 15
        while playback.player?.currentItem?.status != .readyToPlay && ProcessInfo.processInfo.systemUptime < deadline {
            try await Task.sleep(nanoseconds: 20_000_000)
        }
        let player = try XCTUnwrap(playback.player)
        XCTAssertEqual(player.currentItem?.status, .readyToPlay)
        let sought = expectation(description: "Decoded compatibility preview seeks")
        player.seek(to: CMTime(seconds: 0.25, preferredTimescale: 48_000), toleranceBefore: .zero, toleranceAfter: .zero) { finished in
            XCTAssertTrue(finished); sought.fulfill()
        }
        await fulfillment(of: [sought], timeout: 10)
        XCTAssertEqual(player.currentTime().seconds, 0.25, accuracy: 0.05)
        actions.releasePreview(controller: controller)
        try await session.releasePreview(sourceId: source.id)
        XCTAssertFalse(FileManager.default.fileExists(atPath: url.path))
        // Preview retirement must not retire the primary inspection.
        _ = try await session.defaults(sourceId: source.id, operation: .videoConverter)
        try await session.releaseInspection(id: source.id)
        ownedInspections.remove(source.id)
        XCTAssertEqual(try digest(originalURL), original)
    }

    @MainActor
    func testNativePickerPresentationAndCancellationRemainSingleShot() async throws {
        let host = try XCTUnwrap(UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
            .flatMap(\.windows).first(where: \.isKeyWindow)?.rootViewController)
        let actions = IosMediaActions(presentationHost: { host })
        let canceled = expectation(description: "Native picker cancellation")
        var calls = 0
        actions.selectSource(video: true) { source, error in
            calls += 1; XCTAssertNil(source); XCTAssertNil(error); canceled.fulfill()
        }
        let deadline = ProcessInfo.processInfo.systemUptime + 15
        while !(host.presentedViewController is UIDocumentPickerViewController) && ProcessInfo.processInfo.systemUptime < deadline {
            try await Task.sleep(nanoseconds: 20_000_000)
        }
        let picker = try XCTUnwrap(host.presentedViewController as? UIDocumentPickerViewController)
        // UIKit attaches presentedViewController before its presentation has
        // finished. Canceling at that point can tear down DocumentManager's
        // remote host while the service is still installing its controls.
        while (picker.isBeingPresented || picker.transitionCoordinator != nil || picker.viewIfLoaded?.window == nil),
              ProcessInfo.processInfo.systemUptime < deadline {
            try await Task.sleep(nanoseconds: 20_000_000)
        }
        XCTAssertFalse(picker.isBeingPresented)
        XCTAssertNil(picker.transitionCoordinator)
        XCTAssertNotNil(picker.viewIfLoaded?.window)
        XCTAssertFalse(picker.allowsMultipleSelection)
        actions.documentPickerWasCancelled(picker); actions.documentPickerWasCancelled(picker)
        // Keep the picker and its delegate alive through UIKit's actual
        // dismissal acknowledgement, rather than just issuing dismissal.
        await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
            picker.dismiss(animated: false) {
                withExtendedLifetime(actions) { continuation.resume() }
            }
        }
        XCTAssertNil(host.presentedViewController)
        await fulfillment(of: [canceled], timeout: 5)
        XCTAssertEqual(calls, 1)
    }

    override func tearDown() async throws {
        // An unstructured task does not inherit test-task cancellation. Cleanup
        // still awaits real Rust acknowledgements after a canceled test body.
        await Task { @MainActor in await self.cleanupOwnedResources() }.value
        try await super.tearDown()
    }

    @MainActor
    private func cleanupOwnedResources() async {
        let session = AppSession.shared
        var workersSettled = true
        for (id, expectedName) in ownedJobs {
            do { try await session.cancelJob(id: id) }
            catch { XCTFail("Test cleanup could not request cancellation for \(id): \(error)") }
            let deadline = ProcessInfo.processInfo.systemUptime + 45
            var acknowledged = false
            var lastError: Error?
            while ProcessInfo.processInfo.systemUptime < deadline {
                if let job = session.jobs.first(where: { $0.id == id }),
                   [.completed, .failed, .canceled, .interrupted, .targetNotMet].contains(job.state) {
                    if let output = job.output {
                        let directory = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
                            .appendingPathComponent("Velmux", isDirectory: true).standardizedFileURL
                        if job.fileName == expectedName, let url = URL(string: output.locator),
                           url.isFileURL, url.standardizedFileURL.deletingLastPathComponent() == directory,
                           url.lastPathComponent == output.displayName, output.displayName == expectedName {
                            ownedOutputs.insert(url)
                        } else {
                            XCTFail("Test cleanup refused an output outside this test's accepted job ownership: \(id)")
                        }
                    }
                    do {
                        if job.state == .targetNotMet { try await session.discardTarget(id: id) }
                        // Rust rejects dismissal while its worker still owns
                        // resources, even if a terminal row is already visible.
                        try await session.dismissJob(id: id)
                        acknowledged = true
                        break
                    } catch { lastError = error }
                }
                try? await Task.sleep(nanoseconds: 20_000_000)
            }
            if !acknowledged {
                workersSettled = false
                XCTFail("Test cleanup did not receive worker termination/dismissal acknowledgement for \(id): \(String(describing: lastError))")
            }
        }
        // Preserve references and artifacts if Rust cannot confirm termination.
        // Never release a live worker's sources or remove its output resources.
        guard workersSettled else { return }
        ownedJobs.removeAll()
        for id in ownedInspections {
            do { try await session.releaseInspection(id: id) }
            catch { XCTFail("Test cleanup could not release inspection \(id): \(error)") }
        }
        ownedInspections.removeAll()
        var originalsProtected = true
        for (url, expected) in originalHashes {
            do {
                let actual = try digest(url)
                XCTAssertEqual(actual, expected, "The selected original changed during the test")
                if actual != expected { originalsProtected = false }
            } catch {
                originalsProtected = false
                XCTFail("Test cleanup could not verify original protection: \(error)")
            }
        }
        // Assert publication survived core dismissal before removing only the
        // explicitly tracked files this test's accepted jobs created.
        guard originalsProtected else { return }
        for url in ownedOutputs {
            XCTAssertTrue(FileManager.default.fileExists(atPath: url.path), "Core cleanup must preserve the published test result")
            do { try FileManager.default.removeItem(at: url) }
            catch { XCTFail("Test cleanup could not remove its owned published fixture \(url.lastPathComponent): \(error)") }
        }
        ownedOutputs.removeAll(); originalHashes.removeAll()
    }
}

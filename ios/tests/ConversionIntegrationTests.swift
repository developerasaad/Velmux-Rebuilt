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
    private func assertStoredJob(_ job: JobRecord, key: String, normalized: Bool? = nil, gainCentidb: Int32? = nil,
                                 videoRotation: String? = nil, horizontalFlip: Bool? = nil, verticalFlip: Bool? = nil) throws {
        let path = URL(fileURLWithPath: IosServices().storageRoot()).appendingPathComponent("operations.sqlite3").path
        var database: OpaquePointer?
        XCTAssertEqual(sqlite3_open_v2(path, &database, SQLITE_OPEN_READONLY, nil), SQLITE_OK)
        let connection = try XCTUnwrap(database)
        defer { sqlite3_close(connection) }
        sqlite3_busy_timeout(connection, 2_000)
        var statement: OpaquePointer?
        let sql = "SELECT state, record, (SELECT COUNT(*) FROM jobs WHERE submission_key=?2), request FROM jobs WHERE id=?1"
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
        if let normalized {
            let encodedRequest = String(cString: try XCTUnwrap(sqlite3_column_text(query, 3)))
            let request = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(encodedRequest.utf8)) as? [String: Any])
            let plan = try XCTUnwrap(request["plan"] as? [String: Any])
            let configuration = try XCTUnwrap(plan["config"] as? [String: Any])
            let adjustments = try XCTUnwrap(configuration["audio_adjustments"] as? [String: Any])
            XCTAssertEqual(adjustments["normalize_loudness"] as? Bool, normalized)
            if let gainCentidb { XCTAssertEqual((adjustments["gain_centidb"] as? NSNumber)?.int32Value, gainCentidb) }
        }
        if let videoRotation {
            let encodedRequest = String(cString: try XCTUnwrap(sqlite3_column_text(query, 3)))
            let request = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(encodedRequest.utf8)) as? [String: Any])
            let plan = try XCTUnwrap(request["plan"] as? [String: Any])
            let configuration = try XCTUnwrap(plan["config"] as? [String: Any])
            let adjustments = try XCTUnwrap(configuration["video_adjustments"] as? [String: Any])
            XCTAssertEqual(adjustments["rotation"] as? String, videoRotation)
            XCTAssertEqual(adjustments["horizontal_flip"] as? Bool, horizontalFlip)
            XCTAssertEqual(adjustments["vertical_flip"] as? Bool, verticalFlip)
        }
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
        let firstFrameCorners: [[Double]]
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
        var firstFrameCorners: [[Double]] = []
        var previous = -Double.infinity
        while let sample = output.copyNextSampleBuffer() {
            let image = try XCTUnwrap(CMSampleBufferGetImageBuffer(sample))
            if frames == 0 {
                XCTAssertEqual(CVPixelBufferLockBaseAddress(image, .readOnly), kCVReturnSuccess)
                defer { CVPixelBufferUnlockBaseAddress(image, .readOnly) }
                let bytes = try XCTUnwrap(CVPixelBufferGetBaseAddress(image)).assumingMemoryBound(to: UInt8.self)
                let stride = CVPixelBufferGetBytesPerRow(image)
                let width = CVPixelBufferGetWidth(image), height = CVPixelBufferGetHeight(image)
                // Average interior pixels of each quadrant to avoid chroma
                // boundary interpolation and lossy codec edge artifacts.
                for (x, y) in [(width / 4, height / 4), (3 * width / 4, height / 4),
                               (width / 4, 3 * height / 4), (3 * width / 4, 3 * height / 4)] {
                    var rgb = [Double](repeating: 0, count: 3)
                    for dy in -2...2 { for dx in -2...2 {
                        let pixel = bytes.advanced(by: (y + dy) * stride + (x + dx) * 4)
                        rgb[0] += Double(pixel[2]); rgb[1] += Double(pixel[1]); rgb[2] += Double(pixel[0])
                    } }
                    firstFrameCorners.append(rgb.map { $0 / 25 })
                }
            }
            let timestamp = CMSampleBufferGetPresentationTimeStamp(sample).seconds
            XCTAssertTrue(timestamp.isFinite)
            XCTAssertGreaterThanOrEqual(timestamp, previous)
            previous = timestamp
            frames += 1
        }
        XCTAssertEqual(reader.status, .completed, reader.error?.localizedDescription ?? "Video decoding did not finish")
        return VideoProof(frames: frames, width: Int(dimensions.width), height: Int(dimensions.height), duration: asset.duration.seconds, subtype: subtype, firstFrameCorners: firstFrameCorners)
    }

    private struct AudioProof {
        let frames: Int
        let sampleRate: Double
        let channels: Int
        let leftHz: Double
        let rightHz: Double
        let rms: Double
        let samplePeak: Double
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
        var samplePeak = 0.0
        pcm.withUnsafeBytes { bytes in
            for frame in 0..<frames {
                for channel in 0..<channels {
                    let sample = Int16(littleEndian: bytes.loadUnaligned(fromByteOffset: (frame * channels + channel) * 2, as: Int16.self))
                    if previous[channel] <= 0 && sample > 0 { crossings[channel] += 1 }
                    previous[channel] = sample
                    let value = Double(sample) / 32768
                    energy += value * value
                    samplePeak = max(samplePeak, abs(value))
                }
            }
        }
        let seconds = Double(frames) / format.mSampleRate
        return AudioProof(frames: frames, sampleRate: format.mSampleRate, channels: channels,
                          leftHz: Double(crossings[0]) / seconds, rightHz: Double(crossings[1]) / seconds,
                          rms: sqrt(energy / Double(frames * channels)), samplePeak: samplePeak, duration: asset.duration.seconds,
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
    func testVideoRotationAndFlipsReachRealNativeDecodedPixels() async throws {
        let session = try await applicationSession()
        let original = try fixture("orientation-grid.mp4")
        let originalHash = try digest(original)
        originalHashes[original] = originalHash
        let source = try await session.inspect(IosMediaServices.selectedSource(url: original), operation: .videoConverter)
        ownedInspections.insert(source.id)
        let defaults = session.videoAdjustmentDefaults()
        XCTAssertEqual(defaults.rotation, .zero)
        XCTAssertFalse(defaults.horizontalFlip); XCTAssertFalse(defaults.verticalFlip)
        let red = [255.0, 0, 0], green = [0.0, 255, 0]
        let blue = [0.0, 0, 255], yellow = [255.0, 255, 0]
        func assertCorners(_ actual: [[Double]], _ expected: [[Double]]) {
            XCTAssertEqual(actual.count, 4)
            guard actual.count == 4 else { return }
            for index in 0..<4 {
                for channel in 0..<3 {
                    XCTAssertEqual(actual[index][channel], expected[index][channel], accuracy: 35,
                                   "Decoded quadrant \(index), RGB channel \(channel) did not follow the accepted transformation")
                }
            }
        }
        let baseline = try await onWorker { try self.decodeVideo(original) }
        XCTAssertEqual(baseline.width, 160); XCTAssertEqual(baseline.height, 96)
        XCTAssertEqual(baseline.frames, 12)
        assertCorners(baseline.firstFrameCorners, [red, green, blue, yellow])
        var evidence: [String] = []
        let cases: [(VideoRotation, Bool, Bool, Int, Int, [[Double]], String)] = [
            (.clockwise90, true, false, 96, 160, [red, blue, green, yellow], "Clockwise90"),
            (.zero, false, true, 160, 96, [blue, yellow, red, green], "Zero"),
        ]
        for (rotation, horizontal, vertical, width, height, corners, encodedRotation) in cases {
            var configuration = try await session.defaults(sourceId: source.id, operation: .videoConverter)
            configuration.format = .mp4; configuration.videoCodec = .h264
            configuration.fileName = "ios-orientation-" + UUID().uuidString.lowercased()
            var adjustments = defaults
            adjustments.rotation = rotation
            adjustments.horizontalFlip = horizontal; adjustments.verticalFlip = vertical
            configuration.videoAdjustments = adjustments
            let limits = try XCTUnwrap(session.choices(sourceId: source.id, configuration: configuration).videoAdjustmentLimits)
            XCTAssertEqual(Set(limits.rotations), Set([.zero, .clockwise90, .clockwise180, .clockwise270]))
            XCTAssertTrue(limits.canFlipHorizontal); XCTAssertTrue(limits.canFlipVertical)
            var invalidCopy = configuration; invalidCopy.videoCodec = .copy
            do { try await session.validate(sourceId: source.id, configuration: invalidCopy); XCTFail("Rust accepted explicit video copy while pixels must change") }
            catch EngineError.Configuration {}
            let key = configuration.fileName
            let accepted = try await session.submit(sourceId: source.id, key: key, configuration: configuration)
            ownedJobs[accepted.id] = key + ".mp4"
            configuration.videoAdjustments = defaults
            let completed = try await waitForTerminal(session, id: accepted.id)
            let output = try completedURL(completed)
            ownedOutputs.insert(output)
            let proof = try await onWorker {
                try self.assertStoredJob(completed, key: key, videoRotation: encodedRotation,
                                         horizontalFlip: horizontal, verticalFlip: vertical)
                let track = try XCTUnwrap(AVURLAsset(url: output).tracks(withMediaType: .video).first)
                XCTAssertEqual(track.preferredTransform, .identity, "Rotation must be baked into actual pixels without a second display rotation")
                return try self.decodeVideo(output)
            }
            XCTAssertEqual(proof.width, width); XCTAssertEqual(proof.height, height)
            XCTAssertEqual(proof.frames, baseline.frames)
            XCTAssertEqual(proof.subtype, kCMVideoCodecType_H264)
            XCTAssertEqual(proof.duration, baseline.duration, accuracy: 1.0 / 12)
            assertCorners(proof.firstFrameCorners, corners)
            evidence.append("\(rotation), horizontal=\(horizontal), vertical=\(vertical): \(proof.width)×\(proof.height), \(proof.frames) decoded frames, RGB corners=\(proof.firstFrameCorners)")
        }
        let audioURL = try fixture("stereo.wav")
        originalHashes[audioURL] = try digest(audioURL)
        let audio = try await session.inspect(IosMediaServices.selectedSource(url: audioURL), operation: .audioConverter)
        ownedInspections.insert(audio.id)
        var invalidAudio = try await session.defaults(sourceId: audio.id, operation: .audioConverter)
        var activeVideo = defaults; activeVideo.rotation = .clockwise90
        invalidAudio.videoAdjustments = activeVideo
        do { try await session.validate(sourceId: audio.id, configuration: invalidAudio); XCTFail("Rust ignored active video adjustments on Audio Converter") }
        catch EngineError.Configuration {}
        XCTAssertEqual(try digest(original), originalHash)
        let proof = XCTAttachment(string: evidence.joined(separator: "\n"))
        proof.name = "Independent native video orientation pixel verification"
        proof.lifetime = .keepAlways; add(proof)
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
        var adjustments = session.audioAdjustmentDefaults()
        adjustments.gainCentidb = -602
        adjustments.fadeInUs = 500_000; adjustments.fadeOutUs = 500_000
        configuration.audioAdjustments = adjustments
        let choices = try session.choices(sourceId: inspected.id, configuration: configuration)
        XCTAssertEqual(choices.audioAdjustmentLimits?.minimumGainCentidb, -6000)
        var invalid = configuration
        var invalidAdjustments = session.audioAdjustmentDefaults()
        invalidAdjustments.gainCentidb = 2401
        invalid.audioAdjustments = invalidAdjustments
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
    func testAudioSpeedAndPitchReachIndependentNativeDecodedSamples() async throws {
        let session = try await applicationSession()
        let original = try fixture("stereo.wav")
        let originalHash = try digest(original)
        originalHashes[original] = originalHash
        let inspected = try await session.inspect(IosMediaServices.selectedSource(url: original), operation: .audioConverter)
        ownedInspections.insert(inspected.id)
        let defaults = session.audioAdjustmentDefaults()
        XCTAssertEqual(defaults.speedMilli, 1000)
        XCTAssertTrue(defaults.preservePitch)
        var evidence: [String] = []
        for speed: UInt32 in [500, 2000] {
            for preserve in [true, false] {
                var configuration = try await session.defaults(sourceId: inspected.id, operation: .audioConverter)
                configuration.format = .wav; configuration.audioCodec = .pcm
                configuration.sampleRate = 48_000
                configuration.fileName = "ios-speed-" + UUID().uuidString.lowercased()
                var adjustments = defaults
                adjustments.speedMilli = speed; adjustments.preservePitch = preserve
                configuration.audioAdjustments = adjustments
                let choices = try session.choices(sourceId: inspected.id, configuration: configuration)
                XCTAssertEqual(choices.audioAdjustmentLimits?.minimumSpeedMilli, 500)
                XCTAssertEqual(choices.audioAdjustmentLimits?.maximumSpeedMilli, 2000)
                XCTAssertEqual(choices.audioAdjustmentLimits?.maximumFadeUs, 2_000_000_000 / UInt64(speed),
                               "Fade bounds must describe output seconds after speed")
                var invalid = configuration
                var invalidAdjustments = adjustments; invalidAdjustments.speedMilli = 499
                invalid.audioAdjustments = invalidAdjustments
                do { try await session.validate(sourceId: inspected.id, configuration: invalid); XCTFail("Rust accepted audio speed below its limit") }
                catch EngineError.Configuration {}
                let accepted = try await session.submit(sourceId: inspected.id, key: configuration.fileName, configuration: configuration)
                ownedJobs[accepted.id] = configuration.fileName + ".wav"
                let output = try completedURL(try await waitForTerminal(session, id: accepted.id))
                ownedOutputs.insert(output)
                let proof = try await onWorker { try self.decodeAudio(output) }
                let idealFrames = 96_000 * 1000 / Int(speed)
                // atempo uses finite WSOLA windows; this fixture's 48 kHz
                // window is 2048 frames. Bound EOF variation to two windows.
                // Pitch-following asetrate/resample gets only sample rounding.
                let frameTolerance = preserve ? 4096 : 1
                XCTAssertLessThanOrEqual(abs(proof.frames - idealFrames), frameTolerance)
                XCTAssertEqual(proof.sampleRate, 48_000)
                XCTAssertEqual(proof.channels, 2)
                XCTAssertEqual(proof.subtype, kAudioFormatLinearPCM)
                let multiplier = preserve ? 1.0 : Double(speed) / 1000
                XCTAssertEqual(proof.leftHz, 440 * multiplier, accuracy: 8)
                XCTAssertEqual(proof.rightHz, 660 * multiplier, accuracy: 8)
                XCTAssertEqual(proof.duration, Double(proof.frames) / 48_000, accuracy: 0.002)
                XCTAssertEqual(proof.duration, 2 * 1000 / Double(speed), accuracy: Double(frameTolerance) / 48_000 + 0.002)
                XCTAssertGreaterThan(proof.rms, 0.08)
                XCTAssertLessThan(proof.rms, 0.13)
                evidence.append("speed=\(speed)/1000 preservePitch=\(preserve): \(proof.frames) PCM frames, \(proof.duration)s, \(proof.leftHz)/\(proof.rightHz)Hz")
            }
        }
        XCTAssertEqual(try digest(original), originalHash)
        let proof = XCTAttachment(string: evidence.joined(separator: "\n"))
        proof.name = "Independent native audio speed and pitch verification"
        proof.lifetime = .keepAlways; add(proof)
    }

    @MainActor
    func testAudioNormalizationPrecedesManualGainInRealNativeDecodedSamples() async throws {
        let session = try await applicationSession()
        let original = try fixture("stereo.wav")
        let originalHash = try digest(original)
        originalHashes[original] = originalHash
        let source = try await session.inspect(IosMediaServices.selectedSource(url: original), operation: .audioConverter)
        ownedInspections.insert(source.id)
        let baseline = try await onWorker { try self.decodeAudio(original) }
        XCTAssertEqual(baseline.frames, 96_000)
        let defaults = session.audioAdjustmentDefaults()
        XCTAssertFalse(defaults.normalizeLoudness)
        var normalizedRms: Double?
        var evidence: [String] = []
        for gain: Int32 in [0, -602] {
            var configuration = try await session.defaults(sourceId: source.id, operation: .audioConverter)
            configuration.format = .wav; configuration.audioCodec = .pcm
            configuration.sampleRate = 48_000
            configuration.fileName = "ios-normalization-" + UUID().uuidString.lowercased()
            var adjustments = defaults
            adjustments.normalizeLoudness = true; adjustments.gainCentidb = gain
            configuration.audioAdjustments = adjustments
            let choices = try session.choices(sourceId: source.id, configuration: configuration)
            _ = try XCTUnwrap(choices.audioAdjustmentLimits?.loudnessNormalization,
                              "Normalization must expose an actual compiled filter policy")
            let key = configuration.fileName
            let accepted = try await session.submit(sourceId: source.id, key: key, configuration: configuration)
            ownedJobs[accepted.id] = key + ".wav"
            // Later workspace edits must not alter the accepted flag or gain.
            adjustments.normalizeLoudness = false; adjustments.gainCentidb = 0
            configuration.audioAdjustments = adjustments
            let completed = try await waitForTerminal(session, id: accepted.id)
            let output = try completedURL(completed)
            ownedOutputs.insert(output)
            let proof = try await onWorker {
                try self.assertStoredJob(completed, key: key, normalized: true, gainCentidb: gain)
                return try self.decodeAudio(output)
            }
            XCTAssertEqual(proof.frames, baseline.frames)
            XCTAssertEqual(proof.sampleRate, 48_000); XCTAssertEqual(proof.channels, 2)
            XCTAssertEqual(proof.subtype, kAudioFormatLinearPCM)
            XCTAssertEqual(proof.leftHz, 440, accuracy: 1); XCTAssertEqual(proof.rightHz, 660, accuracy: 1)
            XCTAssertEqual(proof.duration, 2, accuracy: 0.002)
            XCTAssertGreaterThan(proof.samplePeak, 0)
            XCTAssertLessThan(proof.samplePeak, 0.3, "This is decoded PCM sample peak, not a true-peak certification")
            if gain == 0 {
                normalizedRms = proof.rms
                XCTAssertGreaterThan(proof.rms, 0.115); XCTAssertLessThan(proof.rms, 0.130)
                XCTAssertGreaterThan(proof.rms, baseline.rms * 1.08)
            } else {
                let expected = try XCTUnwrap(normalizedRms) * pow(10, Double(gain) / 2000)
                XCTAssertEqual(proof.rms, expected, accuracy: 0.001,
                               "Manual gain must remain after normalization rather than being normalized away")
            }
            evidence.append("gain=\(gain)/100 dB: \(proof.frames) PCM frames, RMS=\(proof.rms), samplePeak=\(proof.samplePeak), tones=\(proof.leftHz)/\(proof.rightHz)Hz")
        }
        XCTAssertEqual(try digest(original), originalHash)
        let proof = XCTAttachment(string: evidence.joined(separator: "\n"))
        proof.name = "Independent native normalization sample verification"
        proof.lifetime = .keepAlways; add(proof)
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
        let obsoletePicker = UIDocumentPickerViewController(forOpeningContentTypes: [.data], asCopy: false)
        actions.documentPickerWasCancelled(obsoletePicker)
        XCTAssertEqual(calls, 0, "A callback from another picker must not cancel the active selection")
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

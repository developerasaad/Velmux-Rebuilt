import AVFoundation
import CoreMedia
import CryptoKit
import Darwin
import Foundation

// This is an isolated generated-binding integration test, not a production app.
private struct SmokeFailure: Error, CustomStringConvertible {
    let description: String
    init(_ message: String) { description = message }
}

private func require(_ condition: @autoclosure () throws -> Bool, _ message: String) throws {
    if try condition() == false { throw SmokeFailure(message) }
}

private extension NSLock {
    func locked<T>(_ action: () throws -> T) rethrows -> T {
        lock()
        defer { unlock() }
        return try action()
    }
}

private final class SmokeNativeProvider: NativeProvider, @unchecked Sendable {
    let root: URL
    private let lock = NSLock()
    private var invalidRoot = false
    private var cancelDuringCallback: CancellationToken?
    private var calls: UInt64 = 0
    private var callbackOnMain = false

    init(root: URL) { self.root = root }
    func storageRoot() -> String { lock.locked { invalidRoot ? "relative-root" : root.path } }
    func platformName() -> String { "iOS simulator direct Swift smoke" }
    func onDiagnostic(sequence: UInt64) {
        let token = lock.locked { () -> CancellationToken? in
            calls += 1
            callbackOnMain = callbackOnMain || Thread.isMainThread
            let token = cancelDuringCallback
            cancelDuringCallback = nil
            return token
        }
        token?.cancel()
    }
    func setInvalidRoot(_ value: Bool) { lock.locked { invalidRoot = value } }
    func cancelInNextCallback(_ token: CancellationToken) { lock.locked { cancelDuringCallback = token } }
    var callbackCount: UInt64 { lock.locked { calls } }
    var calledOnMain: Bool { lock.locked { callbackOnMain } }
}

private final class SmokeObserver: JobObserver, @unchecked Sendable {
    private let lock = NSLock()
    private var calls = 0
    private var onMain = false
    func onJobsChanged() { lock.locked { calls += 1; onMain = onMain || Thread.isMainThread } }
    var count: Int { lock.locked { calls } }
    var calledOnMain: Bool { lock.locked { onMain } }
}

/// Real descriptors and real publication into a disposable CI-owned directory.
/// Production document-provider grants/security scope are verified separately.
private final class SmokeMediaProvider: MediaProvider, @unchecked Sendable {
    let root: URL
    let source: URL
    private let lock = NSLock()
    private var denyOpen = false
    private var leases: [String: Int32] = [:]
    private var grants: Set<String> = []
    private var reservations: [String: OutputReservation] = [:]
    private var publications: [String: PublishedOutput] = [:]
    private var releases = 0
    private var descriptorsClosed = true

    init(root: URL, source: URL) { self.root = root; self.source = source }
    func cacheRoot() -> String { root.appendingPathComponent("cache").path }
    func outputLocation() -> String { root.appendingPathComponent("published").path }
    func retainSourceAccess(locator: String) throws {
        try require(locator == source.absoluteString, "The smoke provider must never open unrelated originals")
        lock.locked { _ = grants.insert(locator) }
    }
    func releaseSourceAccess(locator: String) { lock.locked { _ = grants.remove(locator) } }
    func reconcileSourceAccess(retainedLocators: [String]) { lock.locked { grants.formIntersection(retainedLocators) } }
    func failNextOpen() { lock.locked { denyOpen = true } }
    func openSource(locator: String) throws -> OpenedSource {
        let denied = lock.locked { () -> Bool in let denied = denyOpen; denyOpen = false; return denied }
        if denied { throw EngineError.SourceAccess(detail: "Typed Swift provider failure") }
        try require(locator == source.absoluteString, "Unexpected source locator")
        let descriptor = Darwin.open(source.path, O_RDONLY | O_CLOEXEC)
        guard descriptor >= 0 else { throw EngineError.SourceAccess(detail: "The smoke original could not be opened") }
        var info = stat()
        guard fstat(descriptor, &info) == 0, info.st_size > 0 else {
            Darwin.close(descriptor)
            throw EngineError.SourceAccess(detail: "The smoke original has no usable bytes")
        }
        let lease = UUID().uuidString
        lock.locked { leases[lease] = descriptor }
        return OpenedSource(fd: descriptor, offset: 0, length: UInt64(info.st_size), leaseId: lease)
    }
    func releaseSource(leaseId: String) {
        let descriptor = lock.locked { leases.removeValue(forKey: leaseId) }
        if let descriptor {
            errno = 0
            let closed = fcntl(descriptor, F_GETFD) == -1 && errno == EBADF
            lock.locked { releases += 1; descriptorsClosed = descriptorsClosed && closed }
        }
    }
    func reserveOutput(jobId: String, fileName: String, mimeType: String) throws -> OutputReservation {
        try lock.locked {
            if let existing = reservations[jobId] { return existing }
            try require(!fileName.isEmpty && !fileName.contains("/") && !fileName.contains("\\"), "Unsafe reserved name")
            let directory = root.appendingPathComponent("published", isDirectory: true)
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            let locator = directory.appendingPathComponent(jobId + "-" + fileName).absoluteString
            let reserved = OutputReservation(locator: locator, displayName: fileName, mimeType: mimeType)
            reservations[jobId] = reserved
            return reserved
        }
    }
    func publishOutput(reservation: OutputReservation, workingPath: String, expectedSize: UInt64) throws -> PublishedOutput {
        try lock.locked {
            if let existing = publications[reservation.locator] { return existing }
            try require(reservations.values.contains(reservation), "Publication must use an owned reservation")
            let working = URL(fileURLWithPath: workingPath).standardizedFileURL
            try require(working.path.hasPrefix(root.standardizedFileURL.path + "/"), "Working output left the test-owned root")
            try require(expectedSize > 0 && expectedSize <= 8 * 1024 * 1024, "Unexpected smoke output size")
            let bytes = try Data(contentsOf: working)
            try require(UInt64(bytes.count) == expectedSize, "Publication bytes differ from the verified result")
            guard let destination = URL(string: reservation.locator), destination.isFileURL else {
                throw EngineError.Storage(detail: "The smoke publication destination is invalid")
            }
            let pending = destination.deletingLastPathComponent().appendingPathComponent(".pending-" + UUID().uuidString)
            try bytes.write(to: pending, options: .withoutOverwriting)
            defer { try? FileManager.default.removeItem(at: pending) }
            try FileManager.default.moveItem(at: pending, to: destination)
            try require(try Data(contentsOf: destination) == bytes, "Published bytes changed")
            let output = PublishedOutput(locator: destination.absoluteString, displayName: reservation.displayName, size: expectedSize)
            publications[reservation.locator] = output
            return output
        }
    }
    func publicationStatus(reservation: OutputReservation) throws -> PublishedOutput? { lock.locked { publications[reservation.locator] } }
    func discardReservation(reservation: OutputReservation) {
        lock.locked {
            guard publications[reservation.locator] == nil else { return }
            reservations = reservations.filter { $0.value != reservation }
        }
    }
    func nativeVideoCandidates(request: NativeVideoRequest) throws -> [NativeVideoCandidate] { [] }
    var releasedDescriptors: Int { lock.locked { releases } }
    var closedBeforeRelease: Bool { lock.locked { descriptorsClosed } }
    var liveLeases: Int { lock.locked { leases.count } }
    var liveGrants: Int { lock.locked { grants.count } }
}

private func appendLittleEndian<T: FixedWidthInteger>(_ value: T, to data: inout Data) {
    var little = value.littleEndian
    withUnsafeBytes(of: &little) { data.append(contentsOf: $0) }
}

private func writeOriginal(_ url: URL) throws -> Data {
    let rate = 48_000
    let frames = 96_000
    let payload = UInt32(frames * 4)
    var data = Data("RIFF".utf8)
    appendLittleEndian(payload + 36, to: &data)
    data.append(contentsOf: "WAVEfmt ".utf8)
    appendLittleEndian(UInt32(16), to: &data)
    appendLittleEndian(UInt16(1), to: &data)
    appendLittleEndian(UInt16(2), to: &data)
    appendLittleEndian(UInt32(rate), to: &data)
    appendLittleEndian(UInt32(rate * 4), to: &data)
    appendLittleEndian(UInt16(4), to: &data)
    appendLittleEndian(UInt16(16), to: &data)
    data.append(contentsOf: "data".utf8)
    appendLittleEndian(payload, to: &data)
    for frame in 0..<frames {
        for frequency in [440.0, 660.0] {
            appendLittleEndian(Int16((0.15 * sin(2 * Double.pi * frequency * Double(frame) / Double(rate)) * 32767).rounded()), to: &data)
        }
    }
    try data.write(to: url, options: .withoutOverwriting)
    return data
}

private func verifyDecodedAudio(_ url: URL) throws -> [String: Any] {
    let asset = AVURLAsset(url: url)
    guard let track = asset.tracks(withMediaType: .audio).first,
          let description = (track.formatDescriptions as? [CMAudioFormatDescription])?.first,
          let format = CMAudioFormatDescriptionGetStreamBasicDescription(description) else {
        throw SmokeFailure("Published audio has no independently readable native description")
    }
    try require(CMFormatDescriptionGetMediaSubType(description) == kAudioFormatMPEG4AAC, "The output is not actual AAC")
    try require(format.pointee.mSampleRate == 48_000 && format.pointee.mChannelsPerFrame == 2, "Published sampling/layout changed")
    let reader = try AVAssetReader(asset: asset)
    let output = AVAssetReaderTrackOutput(track: track, outputSettings: [
        AVFormatIDKey: kAudioFormatLinearPCM, AVLinearPCMBitDepthKey: 16,
        AVLinearPCMIsFloatKey: false, AVLinearPCMIsBigEndianKey: false,
        AVLinearPCMIsNonInterleaved: false,
    ])
    try require(reader.canAdd(output), "Native AAC decoder cannot attach")
    reader.add(output)
    try require(reader.startReading(), "Native AAC decoder did not start")
    var pcm = Data()
    while let sample = output.copyNextSampleBuffer() {
        guard let block = CMSampleBufferGetDataBuffer(sample) else { throw SmokeFailure("Decoded sample is empty") }
        let length = CMBlockBufferGetDataLength(block)
        var bytes = Data(count: length)
        let status = bytes.withUnsafeMutableBytes {
            CMBlockBufferCopyDataBytes(block, atOffset: 0, dataLength: length, destination: $0.baseAddress!)
        }
        try require(status == noErr, "Decoded PCM could not be read")
        pcm.append(bytes)
        try require(pcm.count <= 8 * 1024 * 1024, "Native decoded smoke output exceeded its bound")
    }
    try require(reader.status == .completed, "Native decoding did not finish: \(String(describing: reader.error))")
    let frames = pcm.count / 4
    try require(abs(frames - 96_000) <= 2048, "AAC output duration/sample count differs unexpectedly")
    var previous = [Int16](repeating: 0, count: 2)
    var crossings = [Int](repeating: 0, count: 2)
    var energy = 0.0
    pcm.withUnsafeBytes { raw in
        for frame in 0..<frames {
            for channel in 0..<2 {
                let sample = Int16(littleEndian: raw.loadUnaligned(fromByteOffset: frame * 4 + channel * 2, as: Int16.self))
                if previous[channel] <= 0 && sample > 0 { crossings[channel] += 1 }
                previous[channel] = sample
                energy += pow(Double(sample) / 32768, 2)
            }
        }
    }
    let seconds = Double(frames) / 48_000
    let left = Double(crossings[0]) / seconds
    let right = Double(crossings[1]) / seconds
    let rms = sqrt(energy / Double(frames * 2))
    try require(abs(left - 440) < 8 && abs(right - 660) < 8, "Actual channel tones were not preserved")
    try require(rms > 0.05 && rms < 0.2, "Decoded output has unexpected sample energy")
    return ["codec": "aac", "sample_rate": 48_000, "channels": 2, "decoded_frames": frames,
            "left_hz": left, "right_hz": right, "rms": rms]
}

private func exercise(_ engine: Engine, provider: SmokeNativeProvider, media: SmokeMediaProvider, observer: SmokeObserver) throws -> [String: Any] {
    try require(!Thread.isMainThread, "Blocking Rust work must execute off the main thread")
    let report = try engine.diagnostics(cancellation: CancellationToken())
    try require(report.ffmpegVersion.hasPrefix("7.1.5"), "The pinned FFmpeg version was not linked")
    try require(report.encoderCount > 0 && report.decoderCount > 0 && report.muxerCount > 0, "Real FFmpeg registries are empty")
    try require(report.storageRoot == provider.root.path && report.sequence == 1, "Generated diagnostic record fields changed")
    let before = provider.callbackCount
    let early = CancellationToken()
    early.cancel()
    do { _ = try engine.diagnostics(cancellation: early); throw SmokeFailure("Pre-call cancellation was ignored") }
    catch EngineError.Cancelled {}
    try require(provider.callbackCount == before, "A canceled call still entered the provider")
    let during = CancellationToken()
    provider.cancelInNextCallback(during)
    do { _ = try engine.diagnostics(cancellation: during); throw SmokeFailure("Callback cancellation was ignored") }
    catch EngineError.Cancelled {}
    try require(provider.callbackCount == before + 1, "Mid-callback cancellation was not executed")
    provider.setInvalidRoot(true)
    do { _ = try engine.diagnostics(cancellation: CancellationToken()); throw SmokeFailure("Invalid storage was accepted") }
    catch EngineError.InvalidStorageRoot {}
    provider.setInvalidRoot(false)
    try require(!provider.calledOnMain, "A diagnostic callback unexpectedly ran on main")
    try engine.initializeMedia(provider: media)
    try engine.observeJobs(observer: observer)
    media.failNextOpen()
    do {
        _ = try engine.inspectSource(locator: media.source.absoluteString, displayName: "original.wav", operation: .audioConverter, cancellation: CancellationToken())
        throw SmokeFailure("A typed Swift callback error was ignored")
    } catch EngineError.SourceAccess(let detail) {
        try require(detail == "Typed Swift provider failure", "The callback's typed error detail changed")
    }
    let inspected = try engine.inspectSource(locator: media.source.absoluteString, displayName: "original.wav", operation: .audioConverter, cancellation: CancellationToken())
    try require(inspected.info.streams.first?.kind == .audio, "Generated stream enum did not describe real audio")
    var configuration = try engine.defaultConfiguration(id: inspected.id, operation: .audioConverter)
    configuration.format = .m4a
    configuration.audioCodec = .aac
    configuration.audioBitrate = 192_000
    configuration.compression = nil
    configuration.fileName = "direct-swift-aac"
    let acceptedConfiguration = configuration
    var invalid = configuration
    invalid.videoCodec = .h264
    do { try engine.validateConfiguration(id: inspected.id, configuration: invalid); throw SmokeFailure("The core accepted video settings for audio") }
    catch EngineError.Configuration(let detail) { try require(!detail.isEmpty, "Typed core validation detail is empty") }
    let key = "direct-swift-" + UUID().uuidString
    let accepted = try engine.submitConversion(id: inspected.id, submissionKey: key, configuration: configuration, cancellation: CancellationToken())
    let duplicate = try engine.submitConversion(id: inspected.id, submissionKey: key, configuration: acceptedConfiguration, cancellation: CancellationToken())
    try require(accepted.id == duplicate.id, "Duplicate submission created another job")
    configuration.fileName = "later-workspace-edit"
    configuration.sampleRate = 44_100
    let deadline = ProcessInfo.processInfo.systemUptime + 60
    var completed: JobRecord?
    while ProcessInfo.processInfo.systemUptime < deadline {
        guard let job = try engine.jobs().first(where: { $0.id == accepted.id }) else { throw SmokeFailure("The accepted job disappeared") }
        if job.state == .completed { completed = job; break }
        if [JobState.failed, .canceled, .interrupted, .targetNotMet].contains(job.state) {
            throw SmokeFailure("Real conversion did not complete: \(job.state), \(job.error ?? "no error detail")")
        }
        Thread.sleep(forTimeInterval: 0.025)
    }
    guard let completed, let published = completed.output, let url = URL(string: published.locator) else {
        throw SmokeFailure("The real conversion did not publish within its bound")
    }
    try require(completed.verified && completed.verifiedSize == published.size && published.size > 0, "Completion preceded verified publication")
    try require(completed.fileName.contains("direct-swift-aac") && !completed.fileName.contains("later-workspace-edit"), "Workspace edits changed the accepted request")
    var decoded = try verifyDecodedAudio(url)
    try require(observer.count > 0 && !observer.calledOnMain, "Real job observer callbacks did not arrive on worker threads")
    try engine.releaseInspection(id: inspected.id)
    try engine.dismissJob(id: completed.id)
    try require(FileManager.default.fileExists(atPath: url.path), "Dismissal removed published output")
    let releaseDeadline = ProcessInfo.processInfo.systemUptime + 5
    while (media.liveLeases != 0 || media.liveGrants != 0) && ProcessInfo.processInfo.systemUptime < releaseDeadline {
        Thread.sleep(forTimeInterval: 0.01)
    }
    try require(media.liveLeases == 0 && media.liveGrants == 0, "Completed resources retain source access")
    try require(media.releasedDescriptors > 0 && media.closedBeforeRelease, "Transferred descriptors were not closed before lease release")
    decoded["publication"] = published.locator
    decoded["bytes"] = published.size
    return ["ffmpeg_version": report.ffmpegVersion, "encoders": report.encoderCount,
            "diagnostic_callbacks": provider.callbackCount, "job_callbacks": observer.count,
            "released_descriptors": media.releasedDescriptors, "media": decoded]
}

private func runSmoke(root: URL) throws -> Data {
    try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
    let ownedRoot = root.resolvingSymlinksInPath()
    let source = ownedRoot.appendingPathComponent("original.wav")
    let original = try writeOriginal(source)
    var provider: SmokeNativeProvider? = SmokeNativeProvider(root: ownedRoot.appendingPathComponent("operations"))
    var media: SmokeMediaProvider? = SmokeMediaProvider(root: ownedRoot, source: source)
    var observer: SmokeObserver? = SmokeObserver()
    weak var weakProvider = provider
    weak var weakMedia = media
    weak var weakObserver = observer
    // Exactly one Engine is constructed for the complete process-owned test.
    var engine: Engine? = Engine(provider: provider!)
    weak var weakEngine = engine
    var proof = try exercise(engine!, provider: provider!, media: media!, observer: observer!)
    try require(try Data(contentsOf: source) == original, "The user-original fixture was modified")
    proof["original_sha256"] = SHA256.hash(data: original).map { String(format: "%02x", $0) }.joined()
    provider = nil
    media = nil
    observer = nil
    try require(weakProvider != nil && weakMedia != nil && weakObserver != nil, "Rust did not retain callback objects for its lifetime")
    engine = nil
    try require(weakEngine == nil, "The final Swift Engine wrapper was retained")
    try require(weakProvider == nil && weakMedia == nil && weakObserver == nil, "Rust callback handles or worker ownership leaked after Engine shutdown")
    proof["single_engine"] = true
    proof["callbacks_released_after_engine"] = true
    proof["pre_call_cancellation"] = true
    proof["mid_callback_cancellation"] = true
    proof["typed_core_and_callback_errors"] = true
    proof["scope"] = "Direct Swift/iOS simulator interoperability and disposable provider publication; production app migration, security-scoped document access and physical hardware are not established."
    return try JSONSerialization.data(withJSONObject: proof, options: [.prettyPrinted, .sortedKeys])
}

@main
private struct SwiftInteropSmoke {
    static func main() async {
        do {
            guard CommandLine.arguments.count == 2 else { throw SmokeFailure("Pass a fresh absolute test-owned root") }
            let root = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true)
            let proof = try await Task.detached(priority: .userInitiated) { try runSmoke(root: root) }.value
            try proof.write(to: root.appendingPathComponent("proof.json"), options: .atomic)
            print(String(decoding: proof, as: UTF8.self))
            print("DIRECT_SWIFT_INTEROP_PASSED")
        } catch {
            fputs("Direct Swift interoperability failed: \(error)\n", stderr)
            exit(1)
        }
    }
}

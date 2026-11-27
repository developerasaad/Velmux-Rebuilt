import CryptoKit
import Darwin
import Foundation
import VelmuxUI

/** Security scope belongs to the lease; the descriptor itself belongs to Rust. */
final class IosSourceLease: NSObject, NSFilePresenter {
    let presentedItemURL: URL?
    let presentedItemOperationQueue: OperationQueue
    private let scopedURL: URL
    private let hasScope: Bool
    private var released = false
    private let lock = NSLock()

    init(url: URL) {
        scopedURL = url
        presentedItemURL = url
        hasScope = url.startAccessingSecurityScopedResource()
        presentedItemOperationQueue = OperationQueue()
        presentedItemOperationQueue.maxConcurrentOperationCount = 1
        super.init()
        NSFileCoordinator.addFilePresenter(self)
    }

    func releaseAccess() {
        lock.lock()
        defer { lock.unlock() }
        guard !released else { return }
        released = true
        NSFileCoordinator.removeFilePresenter(self)
        if hasScope { scopedURL.stopAccessingSecurityScopedResource() }
    }

    deinit { releaseAccess() }
}

/** OS file access and atomic publication only; the Rust database owns jobs and policy. */
final class IosMediaServices: NSObject, MediaServices {
    private static let publicationLock = NSLock()
    private let leaseLock = NSLock()
    private var leases: [String: IosSourceLease] = [:]
    let operationalRoot: URL
    let cacheDirectory: URL
    let outputDirectory: URL
    private let reservationsDirectory: URL

    override convenience init() {
        let files = FileManager.default
        let support = files.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        let caches = files.urls(for: .cachesDirectory, in: .userDomainMask)[0]
        let documents = files.urls(for: .documentDirectory, in: .userDomainMask)[0]
        self.init(
            operationalRoot: support.appendingPathComponent("Velmux", isDirectory: true),
            cacheDirectory: caches.appendingPathComponent("Velmux", isDirectory: true),
            outputDirectory: documents.appendingPathComponent("Velmux", isDirectory: true)
        )
    }

    /** Scoped roots allow simulator tests to exercise production ownership rules. */
    init(operationalRoot: URL, cacheDirectory: URL, outputDirectory: URL) {
        self.operationalRoot = operationalRoot.standardizedFileURL
        self.cacheDirectory = cacheDirectory.standardizedFileURL
        self.outputDirectory = outputDirectory.standardizedFileURL
        reservationsDirectory = operationalRoot.appendingPathComponent("publication-reservations", isDirectory: true)
        super.init()
    }

    func cacheRoot() -> String {
        do {
            try FileManager.default.createDirectory(at: cacheDirectory, withIntermediateDirectories: true)
            return cacheDirectory.path
        } catch { return "" }
    }
    func outputLocation() -> String { outputDirectory.path }

    static func resolveSource(_ locator: String) throws -> URL {
        if locator.hasPrefix("bookmark:") {
            let encoded = String(locator.dropFirst("bookmark:".count))
            guard encoded.utf8.count <= 2_000_000, let data = Data(base64Encoded: encoded) else {
                throw failure("The saved document reference is invalid. Select the source again.")
            }
            var stale = false
            let url: URL
            do {
                url = try URL(resolvingBookmarkData: data, options: [.withoutUI], relativeTo: nil, bookmarkDataIsStale: &stale)
            } catch {
                throw failure("The document can no longer be accessed. Select the source again.")
            }
            guard !stale, url.isFileURL else {
                throw failure("The saved document reference changed. Select the source again.")
            }
            return url
        }
        guard let url = URL(string: locator), url.isFileURL else {
            throw failure("The source does not have a valid native file reference.")
        }
        return url
    }

    static func selectedSource(url: URL) throws -> SelectedSource {
        let lease = IosSourceLease(url: url)
        defer { lease.releaseAccess() }
        var bookmark: Data?
        var coordinationError: NSError?
        var bookmarkError: Error?
        NSFileCoordinator(filePresenter: lease).coordinate(readingItemAt: url, options: [], error: &coordinationError) { coordinated in
            do {
                // iOS document-provider URLs retain their grant in minimal bookmarks.
                bookmark = try coordinated.bookmarkData(options: .minimalBookmark, includingResourceValuesForKeys: nil, relativeTo: nil)
            } catch { bookmarkError = error }
        }
        guard coordinationError == nil, bookmarkError == nil, let data = bookmark else {
            throw failure("The selected document could not be retained for processing.")
        }
        return SelectedSource(locator: "bookmark:" + data.base64EncodedString(), displayName: url.lastPathComponent)
    }

    func openSource(locator: String) throws -> NativeSource {
        let url = try Self.resolveSource(locator)
        let lease = IosSourceLease(url: url)
        var descriptor: Int32 = -1
        var coordinationError: NSError?
        NSFileCoordinator(filePresenter: lease).coordinate(readingItemAt: url, options: [], error: &coordinationError) { coordinated in
            descriptor = Darwin.open(coordinated.path, O_RDONLY | O_CLOEXEC)
        }
        guard coordinationError == nil, descriptor >= 0 else {
            lease.releaseAccess()
            throw Self.failure("The source is unavailable or its document permission was revoked.")
        }
        var info = stat()
        guard fstat(descriptor, &info) == 0, (info.st_mode & S_IFMT) == S_IFREG, info.st_size >= 0 else {
            Darwin.close(descriptor)
            lease.releaseAccess()
            throw Self.failure("The selected source is not an accessible regular file.")
        }
        let leaseID = UUID().uuidString
        leaseLock.lock()
        leases[leaseID] = lease
        leaseLock.unlock()
        // Do not close descriptor here or in releaseSource: ownership crosses into Rust.
        return NativeSource(fd: descriptor, offset: 0, length: info.st_size, leaseId: leaseID)
    }

    func releaseSource(leaseId: String) {
        leaseLock.lock()
        let lease = leases.removeValue(forKey: leaseId)
        leaseLock.unlock()
        lease?.releaseAccess()
    }

    private struct Reservation: Codable {
        var requestedName: String
        var outputName: String
        var mimeType: String
        var device: Int32?
        var inode: UInt64?
        var size: Int64?
        var published: Bool
    }

    private static func identifier(_ jobID: String) throws -> String {
        guard !jobID.isEmpty, jobID.utf8.count <= 200 else { throw failure("The output reservation has an invalid identifier.") }
        return SHA256.hash(data: Data(jobID.utf8)).map { String(format: "%02x", $0) }.joined()
    }

    private func manifestURL(_ locator: String) throws -> URL {
        guard locator.hasPrefix("velmux-reservation:") else { throw Self.failure("The output reservation is invalid.") }
        let identifier = String(locator.dropFirst("velmux-reservation:".count))
        guard identifier.count == 64, identifier.allSatisfy({ $0.isHexDigit && !$0.isUppercase }) else {
            throw Self.failure("The output reservation is invalid.")
        }
        return reservationsDirectory.appendingPathComponent(identifier + ".json")
    }

    private func readReservation(_ reservation: NativeReservation) throws -> Reservation {
        let url = try manifestURL(reservation.locator)
        guard let data = reservationData(url),
              let record = try? JSONDecoder().decode(Reservation.self, from: data),
              record.requestedName == reservation.displayName, record.mimeType == reservation.mimeType,
              Self.validStoredName(record.outputName) else {
            throw Self.failure("The saved output reservation is unavailable or damaged.")
        }
        return record
    }

    private func reservationData(_ url: URL) -> Data? {
        guard let values = try? url.resourceValues(forKeys: [.fileSizeKey, .isRegularFileKey, .isSymbolicLinkKey]),
              values.isRegularFile == true, values.isSymbolicLink != true,
              let size = values.fileSize, size > 0, size < 16_384,
              let data = try? Data(contentsOf: url, options: .mappedIfSafe), data.count < 16_384 else { return nil }
        return data
    }

    private func saveReservation(_ record: Reservation, locator: String) throws {
        let url = try manifestURL(locator)
        try JSONEncoder().encode(record).write(to: url, options: .atomic)
        try syncFile(url)
        syncDirectory(reservationsDirectory)
    }

    static func safeName(_ proposed: String) -> String {
        let invalid = CharacterSet.controlCharacters.union(CharacterSet(charactersIn: "/\\:"))
        var name = proposed.components(separatedBy: invalid).joined(separator: "_")
            .trimmingCharacters(in: .whitespacesAndNewlines)
        while name.hasPrefix(".") { name.removeFirst() }
        if name.isEmpty { name = "converted_media" }
        // Leave space for collision suffixes without splitting Unicode scalars.
        while name.utf8.count > 180 { name.removeLast() }
        return name
    }

    private static func validStoredName(_ name: String) -> Bool {
        let invalid = CharacterSet.controlCharacters.union(CharacterSet(charactersIn: "/\\:"))
        return !name.isEmpty && !name.hasPrefix(".") && name.utf8.count <= 240
            && name.rangeOfCharacter(from: invalid) == nil
    }

    private func availableName(_ name: String, excluding locator: String?) throws -> String {
        var owned = Set<String>()
        for url in try FileManager.default.contentsOfDirectory(at: reservationsDirectory, includingPropertiesForKeys: nil) {
            guard url.pathExtension == "json" else { continue }
            if let locator, url == (try manifestURL(locator)) { continue }
            if let data = reservationData(url),
               let record = try? JSONDecoder().decode(Reservation.self, from: data) {
                owned.insert(record.outputName)
            }
        }
        let stem = (name as NSString).deletingPathExtension
        let suffix = (name as NSString).pathExtension
        for collision in 0..<10_000 {
            let candidate = collision == 0 ? name : stem + " (\(collision))" + (suffix.isEmpty ? "" : "." + suffix)
            if !owned.contains(candidate), !FileManager.default.fileExists(atPath: outputDirectory.appendingPathComponent(candidate).path) {
                return candidate
            }
        }
        throw Self.failure("No unused output name is available. Choose a different name.")
    }

    func reserveOutput(jobId: String, fileName: String, mimeType: String) throws -> NativeReservation {
        Self.publicationLock.lock()
        defer { Self.publicationLock.unlock() }
        do {
            try FileManager.default.createDirectory(at: reservationsDirectory, withIntermediateDirectories: true)
            try FileManager.default.createDirectory(at: outputDirectory, withIntermediateDirectories: true)
            let locator = "velmux-reservation:" + (try Self.identifier(jobId))
            let name = Self.safeName(fileName)
            let result = NativeReservation(locator: locator, displayName: name, mimeType: mimeType)
            if FileManager.default.fileExists(atPath: try manifestURL(locator).path) {
                _ = try readReservation(result)
                return result
            }
            let outputName = try availableName(name, excluding: nil)
            let marker = try manifestURL(locator).appendingPathExtension("reserved")
            let descriptor = Darwin.open(marker.path, O_CREAT | O_EXCL | O_WRONLY | O_CLOEXEC, S_IRUSR | S_IWUSR)
            if descriptor < 0, errno == EEXIST {
                // A crash before the initial manifest commit cannot have published output.
                try FileManager.default.removeItem(at: marker)
                let replacement = Darwin.open(marker.path, O_CREAT | O_EXCL | O_WRONLY | O_CLOEXEC, S_IRUSR | S_IWUSR)
                guard replacement >= 0 else { throw Self.failure("The output destination could not be reserved.") }
                Darwin.close(replacement)
            } else {
                guard descriptor >= 0 else { throw Self.failure("The output destination could not be reserved.") }
                Darwin.close(descriptor)
            }
            do {
                try saveReservation(Reservation(requestedName: name, outputName: outputName, mimeType: mimeType, published: false), locator: locator)
            } catch {
                try? FileManager.default.removeItem(at: marker)
                throw error
            }
            return result
        } catch {
            if (error as NSError).domain == "Velmux.MediaServices" { throw error }
            throw Self.failure("The output destination could not be reserved: \(error.localizedDescription)")
        }
    }

    private func publishedResult(_ record: Reservation) throws -> NativePublished? {
        guard let device = record.device, let inode = record.inode, let size = record.size else { return nil }
        let url = outputDirectory.appendingPathComponent(record.outputName)
        var info = stat()
        guard lstat(url.path, &info) == 0 else {
            if record.published { throw Self.failure("The published file was moved or is no longer available.") }
            return nil
        }
        guard (info.st_mode & S_IFMT) == S_IFREG, info.st_dev == device, info.st_ino == inode, info.st_size == size else {
            if record.published { throw Self.failure("The published file changed outside Velmux.") }
            return nil
        }
        return NativePublished(locator: url.absoluteString, displayName: record.outputName, size: size)
    }

    func publicationStatus(reservation: NativeReservation) throws -> NativePublicationStatus {
        Self.publicationLock.lock()
        defer { Self.publicationLock.unlock() }
        return NativePublicationStatus(output: try publishedResult(readReservation(reservation)))
    }

    func publishOutput(reservation: NativeReservation, workingPath: String, expectedSize: Int64) throws -> NativePublished {
        Self.publicationLock.lock()
        defer { Self.publicationLock.unlock() }
        do {
            var record = try readReservation(reservation)
            if let published = try publishedResult(record) { return published }
            let working = URL(fileURLWithPath: workingPath).standardizedFileURL.resolvingSymlinksInPath()
            let roots = [operationalRoot, cacheDirectory].map { $0.resolvingSymlinksInPath().path + "/" }
            guard roots.contains(where: { working.path.hasPrefix($0) }), expectedSize > 0 else {
                throw Self.failure("Only a verified private working file can be published.")
            }
            var info = stat()
            guard lstat(working.path, &info) == 0, (info.st_mode & S_IFMT) == S_IFREG, info.st_size == expectedSize else {
                throw Self.failure("The verified working result is missing or its size changed.")
            }
            try syncFile(working)
            record.device = info.st_dev
            record.inode = info.st_ino
            record.size = expectedSize
            for _ in 0..<10_000 {
                let destination = outputDirectory.appendingPathComponent(record.outputName)
                // Commit identity before rename so process death after publication is reconcilable.
                try saveReservation(record, locator: reservation.locator)
                if renamex_np(working.path, destination.path, UInt32(RENAME_EXCL)) == 0 {
                    syncDirectory(outputDirectory)
                    record.published = true
                    try saveReservation(record, locator: reservation.locator)
                    return NativePublished(locator: destination.absoluteString, displayName: record.outputName, size: expectedSize)
                }
                guard errno == EEXIST else {
                    throw Self.failure("Saving the result failed. The verified working file is retained for retry.")
                }
                record.outputName = try availableName(record.requestedName, excluding: reservation.locator)
            }
            throw Self.failure("The output name is in use. Choose another name and retry saving.")
        } catch {
            if (error as NSError).domain == "Velmux.MediaServices" { throw error }
            throw Self.failure("Saving the result failed: \(error.localizedDescription). The working result is retained when available.")
        }
    }

    func discardReservation(reservation: NativeReservation) {
        Self.publicationLock.lock()
        defer { Self.publicationLock.unlock() }
        guard let record = try? readReservation(reservation), !record.published,
              (try? publishedResult(record)) == nil, let manifest = try? manifestURL(reservation.locator) else { return }
        // These are ownership metadata only. Never remove a path in Documents.
        try? FileManager.default.removeItem(at: manifest.appendingPathExtension("reserved"))
        try? FileManager.default.removeItem(at: manifest)
    }

    private func syncFile(_ url: URL) throws {
        let descriptor = Darwin.open(url.path, O_RDONLY | O_CLOEXEC)
        guard descriptor >= 0 else { throw Self.failure("A file required for saving is unavailable.") }
        defer { Darwin.close(descriptor) }
        guard fsync(descriptor) == 0 else { throw Self.failure("The file could not be safely finalized on storage.") }
    }

    private func syncDirectory(_ url: URL) {
        let descriptor = Darwin.open(url.path, O_RDONLY | O_CLOEXEC)
        if descriptor >= 0 { _ = fsync(descriptor); Darwin.close(descriptor) }
    }

    static func failure(_ message: String) -> NSError {
        NSError(domain: "Velmux.MediaServices", code: 1, userInfo: [NSLocalizedDescriptionKey: String(message.prefix(320))])
    }
}

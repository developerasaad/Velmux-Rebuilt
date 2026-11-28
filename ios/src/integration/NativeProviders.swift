import Darwin
import Foundation

struct IosSelectedSource: Sendable {
    let locator: String
    let displayName: String
}

// Keep native filesystem operations independent of generated record ownership.
struct NativeSource: Sendable {
    let fd: Int32
    let offset: Int64
    let length: Int64
    let leaseId: String
}

struct NativeReservation: Sendable {
    let locator: String
    let displayName: String
    let mimeType: String
}

struct NativePublished: Sendable {
    let locator: String
    let displayName: String
    let size: Int64
}

struct NativePublicationStatus: Sendable {
    let output: NativePublished?
}

/// One application-owned provider supplies native facilities to the Rust engine.
/// Diagnostic mutations are protected by the lock; callbacks are not main-actor work.
final class IosServices: NSObject, NativeProvider, @unchecked Sendable {
    private let lock = NSLock()
    private var diagnostics: UInt64 = 0
    private var callbackWasMainThread = false

    var lastCallbackWasMainThread: Bool {
        lock.lock()
        defer { lock.unlock() }
        return callbackWasMainThread
    }

    var callbackCount: UInt64 {
        lock.lock()
        defer { lock.unlock() }
        return diagnostics
    }

    func storageRoot() -> String {
        do {
            let root = try FileManager.default.url(
                for: .applicationSupportDirectory,
                in: .userDomainMask,
                appropriateFor: nil,
                create: true
            ).appendingPathComponent("Velmux", isDirectory: true)
            try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
            return root.path
        } catch {
            // Rust rejects an unavailable native storage root with its typed error.
            return ""
        }
    }

    func platformName() -> String { "iOS" }

    func onDiagnostic(sequence: UInt64) {
        lock.lock()
        diagnostics &+= 1
        callbackWasMainThread = Thread.isMainThread
        lock.unlock()
    }
}

/// Direct generated callback adapter. The retained service owns its locked lease
/// table and serialized publication receipts; Rust owns scheduling and storage policy.
final class IosMediaProvider: MediaProvider, @unchecked Sendable {
    let services: IosMediaServices

    init(services: IosMediaServices) { self.services = services }

    func cacheRoot() -> String { services.cacheRoot() }
    func outputLocation() -> String { services.outputLocation() }

    func retainSourceAccess(locator: String) throws {
        try sourceCall { try services.retainSourceAccess(locator: locator) }
    }

    func releaseSourceAccess(locator: String) {
        services.releaseSourceAccess(locator: locator)
    }

    func reconcileSourceAccess(retainedLocators: [String]) {
        services.reconcileSourceAccess(retainedLocators: retainedLocators)
    }

    func openSource(locator: String) throws -> OpenedSource {
        let source = try sourceCall { try services.openSource(locator: locator) }
        guard source.fd >= 0, let offset = UInt64(exactly: source.offset),
              let length = UInt64(exactly: source.length) else {
            // Ownership has not crossed FFI yet. Close the descriptor before
            // acknowledging its native security-scope/file-presenter lease.
            if source.fd >= 0 { Darwin.close(source.fd) }
            services.releaseSource(leaseId: source.leaseId)
            throw EngineError.SourceAccess(detail: "The selected source returned an invalid native descriptor range.")
        }
        return OpenedSource(fd: source.fd, offset: offset, length: length, leaseId: source.leaseId)
    }

    func releaseSource(leaseId: String) { services.releaseSource(leaseId: leaseId) }

    func reserveOutput(jobId: String, fileName: String, mimeType: String) throws -> OutputReservation {
        try storageCall {
            let native = try services.reserveOutput(jobId: jobId, fileName: fileName, mimeType: mimeType)
            return OutputReservation(locator: native.locator, displayName: native.displayName, mimeType: native.mimeType)
        }
    }

    func publishOutput(reservation: OutputReservation, workingPath: String, expectedSize: UInt64) throws -> PublishedOutput {
        try storageCall {
            guard let size = Int64(exactly: expectedSize) else {
                throw EngineError.Storage(detail: "The result exceeds the native storage size limit.")
            }
            return try published(services.publishOutput(reservation: native(reservation), workingPath: workingPath, expectedSize: size))
        }
    }

    func publicationStatus(reservation: OutputReservation) throws -> PublishedOutput? {
        try storageCall {
            guard let output = try services.publicationStatus(reservation: native(reservation)).output else { return nil }
            return try published(output)
        }
    }

    func discardReservation(reservation: OutputReservation) {
        services.discardReservation(reservation: native(reservation))
    }

    func nativeVideoCandidates(request: NativeVideoRequest) throws -> [NativeVideoCandidate] {
        do {
            return try services.nativeVideoCandidates(request: request)
        } catch let error as EngineError {
            throw error
        } catch {
            throw EngineError.Platform(detail: detail(error))
        }
    }

    private func native(_ reservation: OutputReservation) -> NativeReservation {
        NativeReservation(locator: reservation.locator, displayName: reservation.displayName, mimeType: reservation.mimeType)
    }

    private func published(_ output: NativePublished) throws -> PublishedOutput {
        guard let size = UInt64(exactly: output.size) else {
            throw EngineError.Storage(detail: "The native destination returned an invalid output size.")
        }
        return PublishedOutput(locator: output.locator, displayName: output.displayName, size: size)
    }

    private func sourceCall<Value>(_ action: () throws -> Value) throws -> Value {
        do { return try action() }
        catch let error as EngineError { throw error }
        catch { throw EngineError.SourceAccess(detail: detail(error)) }
    }

    private func storageCall<Value>(_ action: () throws -> Value) throws -> Value {
        do { return try action() }
        catch let error as EngineError { throw error }
        catch { throw EngineError.Storage(detail: detail(error)) }
    }

    private func detail(_ error: Error) -> String {
        let message = error.localizedDescription.trimmingCharacters(in: .whitespacesAndNewlines)
        return String((message.isEmpty ? "The native operation could not be completed." : message).prefix(320))
    }
}

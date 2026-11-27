import Foundation
import VelmuxUI

final class IosServices: NSObject, PlatformServices {
    private let lock = NSLock()
    private var diagnostics: Int64 = 0
    private var callbackWasMainThread = false

    var lastCallbackWasMainThread: Bool {
        lock.lock()
        defer { lock.unlock() }
        return callbackWasMainThread
    }

    var callbackCount: Int64 {
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
            // An empty path is rejected by the Rust contract as InvalidStorageRoot.
            return ""
        }
    }

    func platformName() -> String { "iOS" }

    func onDiagnostic(sequence: Int64) {
        lock.lock()
        diagnostics += 1
        callbackWasMainThread = Thread.isMainThread
        lock.unlock()
    }
}

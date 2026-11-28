import SwiftUI
import UIKit

/// The native player retains its document lease until dismantling acknowledges
/// release. Preparing a fallback is a separate explicit core request.
struct MediaPreview: UIViewControllerRepresentable {
    let locator: String
    let actions: IosMediaActions
    let onUnsupported: () -> Void
    let handle: MediaPreviewHandle

    func makeUIViewController(context: Context) -> UIViewController {
        let controller = actions.createPreview(locator: locator, onUnsupported: onUnsupported)
        handle.controller = controller
        return controller
    }

    func updateUIViewController(_ controller: UIViewController, context: Context) {}

    static func dismantleUIViewController(_ controller: UIViewController, coordinator: ()) {
        // The controller owns its native player; delegate explicit release via
        // the same provider before SwiftUI lets the controller leave scope.
        (controller as? NativePlayerReleasing)?.releaseNativePlayer()
    }
}

@MainActor
protocol NativePlayerReleasing: AnyObject {
    func releaseNativePlayer()
}

/// Native playback is closed synchronously before releasing core-owned preview
/// artifacts or leaving the source workspace. It never owns the Rust engine.
@MainActor
final class MediaPreviewHandle: ObservableObject {
    weak var controller: UIViewController?
    func release() {
        (controller as? NativePlayerReleasing)?.releaseNativePlayer()
        controller = nil
    }
}

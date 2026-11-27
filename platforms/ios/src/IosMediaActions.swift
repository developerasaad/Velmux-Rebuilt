import AVFoundation
import AVKit
import UIKit
import UniformTypeIdentifiers
import VelmuxUI

/** UIKit actions are confined to the main queue; file access runs on workers. */
final class IosMediaActions: NSObject, PlatformActions, NativePreviewProvider, UIDocumentPickerDelegate, UIDocumentInteractionControllerDelegate, UIAdaptivePresentationControllerDelegate {
    private let presentationHost: () -> UIViewController?
    private var selection: ((SelectedSource?, String?) -> Void)?
    private var selectionID: UUID?
    private var resolvingSelection = false
    private var documentInteraction: UIDocumentInteractionController?

    init(presentationHost: @escaping () -> UIViewController?) {
        self.presentationHost = presentationHost
    }

    private func presenter() -> UIViewController? {
        var host = presentationHost()
        while let presented = host?.presentedViewController, !presented.isBeingDismissed { host = presented }
        return host
    }

    func selectSource(video: Bool, completion: @escaping (SelectedSource?, String?) -> Void) {
        DispatchQueue.main.async {
            guard self.selection == nil else {
                _ = completion(nil, "A document selection is already open.")
                return
            }
            guard let presenter = self.presenter(), presenter.viewIfLoaded?.window != nil else {
                _ = completion(nil, "The document picker is unavailable while the application is not visible.")
                return
            }
            let picker = UIDocumentPickerViewController(forOpeningContentTypes: [video ? UTType.movie : UTType.audio], asCopy: false)
            picker.allowsMultipleSelection = false
            picker.shouldShowFileExtensions = true
            picker.delegate = self
            self.selection = completion
            self.selectionID = UUID()
            presenter.present(picker, animated: true)
            picker.presentationController?.delegate = self
        }
    }

    func documentPickerWasCancelled(_ controller: UIDocumentPickerViewController) {
        let completion = selection
        selection = nil
        selectionID = nil
        resolvingSelection = false
        _ = completion?(nil, nil)
    }

    func presentationControllerDidDismiss(_ presentationController: UIPresentationController) {
        if !resolvingSelection, let picker = presentationController.presentedViewController as? UIDocumentPickerViewController {
            documentPickerWasCancelled(picker)
        }
    }

    func documentPicker(_ controller: UIDocumentPickerViewController, didPickDocumentsAt urls: [URL]) {
        guard let url = urls.first, let identifier = selectionID else {
            documentPickerWasCancelled(controller)
            return
        }
        resolvingSelection = true
        // Retain the picker-granted scope before crossing into the background queue.
        let lease = IosSourceLease(url: url)
        DispatchQueue.global(qos: .userInitiated).async {
            defer { lease.releaseAccess() }
            let result: Result<SelectedSource, Error> = Result { try IosMediaServices.selectedSource(url: url) }
            DispatchQueue.main.async {
                guard self.selectionID == identifier else { return }
                let completion = self.selection
                self.selection = nil
                self.selectionID = nil
                self.resolvingSelection = false
                switch result {
                case .success(let source): _ = completion?(source, nil)
                case .failure(let error): _ = completion?(nil, String(error.localizedDescription.prefix(320)))
                }
            }
        }
    }

    func openOutput(locator: String, completion: @escaping (String?) -> Void) {
        DispatchQueue.main.async {
            guard let url = URL(string: locator), url.isFileURL,
                  FileManager.default.isReadableFile(atPath: url.path),
                  let presenter = self.presenter() else {
                _ = completion("The saved result is no longer accessible. Locate it in Files if it was moved.")
                return
            }
            let document = UIDocumentInteractionController(url: url)
            document.delegate = self
            self.documentInteraction = document
            if !document.presentPreview(animated: true) {
                guard document.presentOptionsMenu(from: presenter.view.bounds, in: presenter.view, animated: true) else {
                    _ = completion("No installed application can open this result.")
                    return
                }
            }
            _ = completion(nil)
        }
    }

    func shareOutput(locator: String, completion: @escaping (String?) -> Void) {
        DispatchQueue.main.async {
            guard let url = URL(string: locator), url.isFileURL,
                  FileManager.default.isReadableFile(atPath: url.path),
                  let presenter = self.presenter() else {
                _ = completion("The saved result is no longer accessible. Locate it in Files if it was moved.")
                return
            }
            let share = UIActivityViewController(activityItems: [url], applicationActivities: nil)
            if let popover = share.popoverPresentationController {
                popover.sourceView = presenter.view
                popover.sourceRect = CGRect(x: presenter.view.bounds.midX, y: presenter.view.bounds.midY, width: 1, height: 1)
                popover.permittedArrowDirections = []
            }
            presenter.present(share, animated: true)
            _ = completion(nil)
        }
    }

    func documentInteractionControllerViewControllerForPreview(_ controller: UIDocumentInteractionController) -> UIViewController {
        presentationHost() ?? UIViewController()
    }

    func documentInteractionControllerDidEndPreview(_ controller: UIDocumentInteractionController) {
        if documentInteraction === controller { documentInteraction = nil }
    }

    func createPreview(locator: String, onUnsupported: @escaping () -> Void) -> UIViewController {
        dispatchPrecondition(condition: .onQueue(.main))
        do {
            let url = try IosMediaServices.resolveSource(locator)
            return IosPlayerController(url: url) { _ = onUnsupported() }
        } catch {
            DispatchQueue.main.async { _ = onUnsupported() }
            return UIViewController()
        }
    }

    func releasePreview(controller: UIViewController) {
        dispatchPrecondition(condition: .onQueue(.main))
        (controller as? IosPlayerController)?.releasePlayer()
    }
}

/** AVPlayer supplies real playback, scrubbing and seeking without proxy generation. */
private final class IosPlayerController: UIViewController {
    private let playback = AVPlayerViewController()
    private var lease: IosSourceLease?
    private var statusObservation: NSKeyValueObservation?
    private var unsupported: (() -> Void)?
    private var released = false

    init(url: URL, onUnsupported: @escaping () -> Void) {
        unsupported = onUnsupported
        lease = IosSourceLease(url: url)
        super.init(nibName: nil, bundle: nil)
        var coordinationError: NSError?
        var asset: AVURLAsset?
        NSFileCoordinator(filePresenter: lease).coordinate(readingItemAt: url, options: [], error: &coordinationError) { coordinated in
            asset = AVURLAsset(url: coordinated)
        }
        if coordinationError == nil, let asset {
            let item = AVPlayerItem(asset: asset)
            playback.player = AVPlayer(playerItem: item)
            playback.showsPlaybackControls = true
            playback.videoGravity = .resizeAspect
            statusObservation = item.observe(\.status, options: [.initial, .new]) { [weak self] item, _ in
                if item.status == .failed {
                    DispatchQueue.main.async { self?.reportUnsupported() }
                }
            }
        } else {
            DispatchQueue.main.async { [weak self] in self?.reportUnsupported() }
        }
    }

    required init?(coder: NSCoder) { fatalError("Storyboards are not supported.") }

    override func viewDidLoad() {
        super.viewDidLoad()
        addChild(playback)
        view.addSubview(playback.view)
        playback.view.translatesAutoresizingMaskIntoConstraints = false
        NSLayoutConstraint.activate([
            playback.view.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            playback.view.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            playback.view.topAnchor.constraint(equalTo: view.topAnchor),
            playback.view.bottomAnchor.constraint(equalTo: view.bottomAnchor),
        ])
        playback.didMove(toParent: self)
    }

    private func reportUnsupported() {
        guard !released else { return }
        let callback = unsupported
        unsupported = nil
        callback?()
    }

    func releasePlayer() {
        guard !released else { return }
        released = true
        unsupported = nil
        statusObservation?.invalidate()
        statusObservation = nil
        playback.player?.pause()
        playback.player?.replaceCurrentItem(with: nil)
        playback.player = nil
        lease?.releaseAccess()
        lease = nil
    }

    deinit {
        statusObservation?.invalidate()
        lease?.releaseAccess()
    }
}

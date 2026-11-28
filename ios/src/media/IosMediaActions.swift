import AVFoundation
import Combine
import AVKit
import UIKit
import UniformTypeIdentifiers

/** UIKit actions are confined to the main queue; file access runs on workers. */
final class IosMediaActions: NSObject, ObservableObject, UIDocumentPickerDelegate, UIDocumentInteractionControllerDelegate, UIAdaptivePresentationControllerDelegate {
    private let presentationHost: () -> UIViewController?
    private var selection: ((IosSelectedSource?, String?) -> Void)?
    private var selectionID: UUID?
    private weak var activePicker: UIDocumentPickerViewController?
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

    func selectSource(video: Bool, completion: @escaping (IosSelectedSource?, String?) -> Void) {
        DispatchQueue.main.async {
            guard self.selection == nil else {
                _ = completion(nil, "A document selection is already open.")
                return
            }
            guard let presenter = self.presenter(), presenter.viewIfLoaded?.window != nil else {
                _ = completion(nil, "The document picker is unavailable while the application is not visible.")
                return
            }
            // Providers can advertise valid media as generic data (including
            // containers the native player cannot decode). Rust inspects actual
            // bytes and validates the selected operation after document access.
            let picker = UIDocumentPickerViewController(forOpeningContentTypes: [.data], asCopy: false)
            picker.title = video ? "Choose a video source" : "Choose an audio source"
            picker.navigationItem.prompt = "The selected file's contents are inspected."
            picker.allowsMultipleSelection = false
            picker.shouldShowFileExtensions = true
            picker.delegate = self
            self.selection = completion
            self.selectionID = UUID()
            self.activePicker = picker
            presenter.present(picker, animated: true)
            picker.presentationController?.delegate = self
        }
    }

    func documentPickerWasCancelled(_ controller: UIDocumentPickerViewController) {
        guard activePicker === controller else { return }
        let completion = selection
        selection = nil
        selectionID = nil
        activePicker = nil
        resolvingSelection = false
        _ = completion?(nil, nil)
    }

    func presentationControllerDidDismiss(_ presentationController: UIPresentationController) {
        if !resolvingSelection, let picker = presentationController.presentedViewController as? UIDocumentPickerViewController {
            documentPickerWasCancelled(picker)
        }
    }

    func documentPicker(_ controller: UIDocumentPickerViewController, didPickDocumentsAt urls: [URL]) {
        guard activePicker === controller else { return }
        guard let url = urls.first, let identifier = selectionID else {
            documentPickerWasCancelled(controller)
            return
        }
        resolvingSelection = true
        // Retain the picker-granted scope before crossing into the background queue.
        let lease = IosSourceLease(url: url)
        DispatchQueue.global(qos: .userInitiated).async {
            defer { lease.releaseAccess() }
            let result: Result<IosSelectedSource, Error> = Result { try IosMediaServices.selectedSource(url: url) }
            DispatchQueue.main.async {
                guard self.selectionID == identifier else { return }
                let completion = self.selection
                self.selection = nil
                self.selectionID = nil
                self.activePicker = nil
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
        return IosPlayerController(locator: locator, onUnsupported: onUnsupported)
    }

    func releasePreview(controller: UIViewController) {
        dispatchPrecondition(condition: .onQueue(.main))
        (controller as? IosPlayerController)?.releasePlayer()
    }
}

/** Owns pending coordination independently of the controller's main-thread lifetime. */
private final class IosPreviewPreparation {
    struct Prepared {
        let asset: AVURLAsset
        let lease: IosSourceLease
    }
    private let lock = NSLock()
    private var canceled = false
    private var coordinator: NSFileCoordinator?
    private let accessQueue: OperationQueue = {
        let queue = OperationQueue()
        queue.maxConcurrentOperationCount = 1
        queue.qualityOfService = .userInitiated
        return queue
    }()

    func prepare(locator: String, completion: @escaping (Prepared?) -> Void) {
        dispatchPrecondition(condition: .notOnQueue(.main))
        lock.lock()
        let stopped = canceled
        lock.unlock()
        guard !stopped, let url = try? IosMediaServices.resolveSource(locator) else { completion(nil); return }
        let lease = IosSourceLease(url: url)
        let pending = NSFileCoordinator(filePresenter: lease)
        lock.lock()
        if canceled {
            lock.unlock()
            lease.releaseAccess()
            completion(nil)
            return
        }
        coordinator = pending
        let intent = NSFileAccessIntent.readingIntent(with: url, options: [])
        // The asynchronous request is submitted under the cancellation lock, so
        // cancellation cannot slip between registration and request submission.
        pending.coordinate(with: [intent], queue: accessQueue) { [self] error in
            lock.lock()
            coordinator = nil
            let discarded = canceled
            lock.unlock()
            guard !discarded, error == nil else {
                lease.releaseAccess()
                completion(nil)
                return
            }
            completion(Prepared(asset: AVURLAsset(url: intent.url), lease: lease))
        }
        lock.unlock()
    }

    func cancel() {
        lock.lock()
        canceled = true
        let pending = coordinator
        lock.unlock()
        // Foundation permits cancellation from any thread without waiting for
        // coordination to finish. The worker keeps its lease until it returns.
        pending?.cancel()
    }
}

/** AVPlayer supplies real playback, scrubbing and seeking without proxy generation. */
private final class IosPlayerController: UIViewController, NativePlayerReleasing {
    private let playback = AVPlayerViewController()
    private var lease: IosSourceLease?
    private var statusObservation: NSKeyValueObservation?
    private var unsupported: (() -> Void)?
    private var released = false
    private var preparation: IosPreviewPreparation?

    init(locator: String, onUnsupported: @escaping () -> Void) {
        unsupported = onUnsupported
        super.init(nibName: nil, bundle: nil)
        let pending = IosPreviewPreparation()
        preparation = pending
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            pending.prepare(locator: locator) { result in
                DispatchQueue.main.async { [weak self] in
                    guard let self, !self.released, self.preparation === pending else {
                        result?.lease.releaseAccess()
                        return
                    }
                    self.preparation = nil
                    guard let result else { self.reportUnsupported(); return }
                    self.attach(result)
                }
            }
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + 30) { [weak self, weak pending] in
            guard let self, let pending, !self.released, self.preparation === pending else { return }
            self.preparation = nil
            pending.cancel()
            self.reportUnsupported()
        }
    }

    private func attach(_ prepared: IosPreviewPreparation.Prepared) {
        dispatchPrecondition(condition: .onQueue(.main))
        lease = prepared.lease
        let item = AVPlayerItem(asset: prepared.asset)
        playback.player = AVPlayer(playerItem: item)
        playback.showsPlaybackControls = true
        playback.videoGravity = .resizeAspect
        statusObservation = item.observe(\.status, options: [.initial, .new]) { [weak self] item, _ in
            if item.status == .failed {
                DispatchQueue.main.async { self?.reportUnsupported() }
            }
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

    func releaseNativePlayer() { releasePlayer() }

    func releasePlayer() {
        guard !released else { return }
        released = true
        unsupported = nil
        preparation?.cancel()
        preparation = nil
        statusObservation?.invalidate()
        statusObservation = nil
        playback.player?.pause()
        playback.player?.replaceCurrentItem(with: nil)
        playback.player = nil
        lease?.releaseAccess()
        lease = nil
    }

    deinit {
        preparation?.cancel()
        statusObservation?.invalidate()
        lease?.releaseAccess()
    }
}

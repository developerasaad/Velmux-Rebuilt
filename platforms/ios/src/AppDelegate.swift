import UIKit
import VelmuxUI

@main
final class AppDelegate: UIResponder, UIApplicationDelegate {
    var window: UIWindow?
    private(set) var session: FoundationSession?
    private(set) var mediaActions: IosMediaActions?

    func application(
        _ application: UIApplication,
        didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]?
    ) -> Bool {
        let session = FoundationSession(services: IosServices())
        self.session = session
        do {
            try session.media.attach(services: IosMediaServices())
        } catch {
            NSLog("Native media services could not be attached: %@", String(describing: error))
        }
        let actions = IosMediaActions(presentationHost: { [weak self] in self?.window?.rootViewController })
        self.mediaActions = actions
        ControllerKt.installNativePreviewProvider(provider: actions)
        let window = UIWindow(frame: UIScreen.main.bounds)
        window.rootViewController = ControllerKt.createController(session: session, actions: actions)
        window.makeKeyAndVisible()
        self.window = window
        return true
    }

    func applicationWillTerminate(_ application: UIApplication) {
        session?.close()
        session = nil
    }
}

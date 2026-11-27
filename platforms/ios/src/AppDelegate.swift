import UIKit
import VelmuxUI

@main
final class AppDelegate: UIResponder, UIApplicationDelegate {
    var window: UIWindow?
    private var session: FoundationSession?

    func application(
        _ application: UIApplication,
        didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]?
    ) -> Bool {
        let session = FoundationSession(services: IosServices())
        self.session = session
        let window = UIWindow(frame: UIScreen.main.bounds)
        window.rootViewController = ControllerKt.createController(session: session)
        window.makeKeyAndVisible()
        self.window = window
        return true
    }

    func applicationWillTerminate(_ application: UIApplication) {
        session?.close()
        session = nil
    }
}

import UIKit

/// Simulator-only Files fixture host. CI supplies generated media in Documents;
/// this app contains no Velmux engine, media code, or URL delivery hooks.
@main
final class MediaFixturesApp: UIResponder, UIApplicationDelegate {
    var window: UIWindow?

    func application(_ application: UIApplication, didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]?) -> Bool {
        window = UIWindow(frame: UIScreen.main.bounds)
        window?.rootViewController = UIViewController()
        window?.makeKeyAndVisible()
        return true
    }
}

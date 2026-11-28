import SwiftUI
import UIKit

@main
@MainActor
struct VelmuxApp: App {
    @StateObject private var session = AppSession.shared
    @StateObject private var actions = IosMediaActions(presentationHost: {
        UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
            .flatMap(\.windows).first(where: \.isKeyWindow)?.rootViewController
    })

    var body: some Scene {
        WindowGroup {
            VelmuxRootView(session: session, actions: actions)
                .preferredColorScheme(session.appearance == "dark" ? .dark : session.appearance == "light" ? .light : nil)
                .tint(.primary)
        }
    }
}

import SwiftUI

@main
struct LiveStageApp: App {
    var body: some Scene {
        WindowGroup {
            ContentView()
        }
        #if os(macOS)
            Settings {
                SettingView()
            }
            .windowResizability(.contentSize)
            .windowStyle(.hiddenTitleBar)
        #endif
    }
}

import SwiftUI

@main
struct TeamCraftApp: App {
  @UIApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
  @State private var model = AppModel.shared
  @Environment(\.scenePhase) private var scenePhase

  var body: some Scene {
    WindowGroup {
      RootView()
        .environment(model)
        .tint(Theme.accent)
        .task { await model.bootstrap() }
        .onChange(of: scenePhase) { _, phase in
          Task {
            switch phase {
            case .active: await model.sceneBecameActive()
            case .background: await model.sceneEnteredBackground()
            default: break
            }
          }
        }
    }
  }
}

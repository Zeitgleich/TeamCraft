import SwiftUI

@main
struct TeamCraftApp: App {
  @UIApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
  @State private var model = AppModel.shared
  @State private var lock = AppLock()
  @AppStorage(AppLock.privacyCoverKey) private var isPrivacyCoverOn = false
  @Environment(\.scenePhase) private var scenePhase

  var body: some Scene {
    WindowGroup {
      RootView()
        .environment(model)
        .overlay {
          if lock.isLocked {
            LockView(lock: lock)
          } else if isPrivacyCoverOn && scenePhase != .active {
            PrivacyCover()
          }
        }
        .tint(Theme.accent)
        .task { await model.bootstrap() }
        .onChange(of: scenePhase) { _, phase in
          switch phase {
          case .active: lock.didBecomeActive()
          case .background: lock.didEnterBackground()
          default: break
          }
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

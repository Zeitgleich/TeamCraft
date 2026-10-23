import SwiftUI

@main
struct TeamCraftApp: App {
  @UIApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
  @State private var model = AppModel.shared
  @State private var lock = AppLock()
  @State private var shield = ShieldWindow()
  @Environment(\.scenePhase) private var scenePhase

  var body: some Scene {
    WindowGroup {
      RootView()
        .environment(model)
        .tint(Theme.accent)
        .task {
          updateShield(phase: scenePhase)
          await model.bootstrap()
        }
        .onChange(of: lock.isLocked) { _, _ in updateShield(phase: scenePhase) }
        .onChange(of: scenePhase) { _, phase in
          switch phase {
          case .active: lock.didBecomeActive()
          case .background: lock.didEnterBackground()
          default: break
          }
          updateShield(phase: phase)
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

  /// The lock screen while locked; otherwise the cover while not active, when wanted.
  private func updateShield(phase: ScenePhase) {
    if lock.isLocked {
      shield.show(.lock, lock: lock)
    } else if lock.isCoverWanted && phase != .active {
      shield.show(.cover, lock: lock)
    } else {
      shield.show(nil, lock: lock)
    }
  }
}

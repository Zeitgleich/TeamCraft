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
          // A cold launch may never report a change of `scenePhase` to active (`onChange` below).
          model.loadWaitingShare()
          await model.bootstrap()
        }
        // Universal Links and `teamcraft://` alike; waits behind the app lock's shield and the sign-in.
        .onOpenURL { url in model.receiveAppLink(url) }
        .onChange(of: lock.isLocked) { _, _ in updateShield(phase: scenePhase) }
        .onChange(of: scenePhase) { _, phase in
          switch phase {
          case .active:
            lock.didBecomeActive()
            // The share extension may have left something, while the app slept or wasn't running.
            model.loadWaitingShare()
            model.reportActivity(.foreground)
          case .background:
            // First, before anything else runs: the background time for pending sends (#158).
            model.sceneWillEnterBackground()
            lock.didEnterBackground()
            model.reportActivity(.background)
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

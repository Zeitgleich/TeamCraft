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
        .environment(lock)
        .tint(Theme.accent)
        .task {
          updateShield(phase: scenePhase)
          // Voice takes left by a crash or a killed app are never sent; decoded copies go too (#204).
          VoiceFolders.clearAll()
          // A cold launch may never report a change of `scenePhase` to active (`onChange` below).
          model.loadWaitingShare()
          await model.bootstrap()
        }
        // Universal Links and `teamcraft://` alike; waits behind the app lock's shield and the sign-in.
        .onOpenURL { url in model.receiveAppLink(url) }
        .onChange(of: lock.isLocked) { _, isLocked in
          updateShield(phase: scenePhase)
          if isLocked { stopVoice(.background) }
        }
        // A call goes first: a recording stops (its take stays), a voice message pauses (#204).
        .onChange(of: model.calls.activeCall?.id) { _, maybeCall in
          if maybeCall != nil { stopVoice(.call) }
        }
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
            stopVoice(.background)
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

  /// Stops a voice recording (the take stays for the user) and pauses a voice message.
  private func stopVoice(_ reason: VoiceStop) {
    VoiceRecorder.shared.interrupt(reason)
    VoicePlayer.shared.pause()
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

import AVFoundation
import UIKit

/// The screen goes off at the ear during a call, as in the Phone app: while a call runs on the
/// built-in receiver, proximity monitoring is on, and iOS turns the screen off whenever the phone
/// is near the face. The loudspeaker, headsets and Bluetooth keep the screen on, and so does an
/// incoming call that only rings (it has to be answered). Voice messages turn the sensor on by
/// themselves (`VoiceProximity`); this only switches it off again when it switched it on.
@MainActor
final class CallProximity {
  private var isOn = false
  private var routeObserver: NSObjectProtocol?
  private var maybeCall: CallProximityState?

  init() {
    routeObserver = NotificationCenter.default.addObserver(
      forName: AVAudioSession.routeChangeNotification, object: nil, queue: .main
    ) { [weak self] _ in
      MainActor.assumeIsolated { self?.apply() }
    }
  }

  /// The call as it is now; `nil` once it is over.
  func follow(_ call: CallProximityState?) {
    maybeCall = call
    apply()
  }

  private func apply() {
    let isOnReceiver = AVAudioSession.sharedInstance().currentRoute.outputs.contains { $0.portType == .builtInReceiver }
    let wantsOn = Self.isScreenOffAtEar(maybeCall, isOnReceiver: isOnReceiver)
    guard wantsOn != isOn else { return }
    isOn = wantsOn
    UIDevice.current.isProximityMonitoringEnabled = wantsOn
  }

  /// Only a call this device is in (or calls out with) whose sound comes from the receiver.
  nonisolated static func isScreenOffAtEar(_ maybeCall: CallProximityState?, isOnReceiver: Bool) -> Bool {
    guard let call = maybeCall, isOnReceiver else { return false }
    return !call.isRingingIn
  }
}

/// What the screen-off at the ear needs to know about the call.
struct CallProximityState: Equatable {
  /// An incoming call that rings and wasn't answered yet.
  let isRingingIn: Bool
}

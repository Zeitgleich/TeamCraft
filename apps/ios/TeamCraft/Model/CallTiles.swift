import Foundation
import SwiftUI

/// What VoiceOver says for a person's call tile, as web and desktop put it together: the name, then
/// "muted", then "not verified, gets no audio or video" when one of their devices' call key binding
/// did not verify (#1771, `CallStateInfo.unverifiedUserIds`): that device is kept out of the call's
/// encryption group.
struct CallTileLabel: Equatable {
  let name: String
  let isMuted: Bool
  let isUnverified: Bool

  var text: Text { Text(verbatim: spoken()) }

  /// The label in the language of `bundle` (the app's own by default).
  func spoken(in bundle: Bundle = .main) -> String {
    switch (isMuted, isUnverified) {
    case (true, true): return String(localized: "call_person_muted_unverified \(name)", bundle: bundle)
    case (false, true): return String(localized: "call_person_unverified \(name)", bundle: bundle)
    case (true, false): return String(localized: "call_person_muted \(name)", bundle: bundle)
    case (false, false): return name
    }
  }
}

extension RemoteVideoStream {
  init(track: RemoteVideoTrack, renderer: RemoteVideoRenderer) {
    self.init(
      id: track.streamId, deviceId: track.deviceId, isScreen: track.isScreen, isPlayable: track.isPlayable,
      renderer: renderer)
  }

  /// What the tile says instead of a picture: the track comes in a codec the phone doesn't decode
  /// (#1838, a browser that can't encode H.264 sends AV1 or VP9). Such a track is not watched; the
  /// SFU would refuse it anyway.
  var maybeNotice: LocalizedStringKey? {
    isPlayable ? nil : "call_video_undecodable"
  }
}

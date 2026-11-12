import SwiftUI

/// A space's picture as a small rounded square, or nothing without one (so the row keeps its
/// current look), while it loads and when it can't be loaded. It is fetched only while on screen; one
/// that failed during an outage is asked for again in the next shared retry round (#1617), and what
/// is shown meanwhile stays.
struct SpacePictureView: View {
  @Environment(AppModel.self) private var model
  let spaceId: String
  let maybeVersion: String?
  var size: CGFloat = 24

  @State private var maybeImage: UIImage?
  @State private var isWaitingForRetry = false
  /// Counts the retries of this picture; a new one loads it again.
  @State private var attempt = 0

  var body: some View {
    Group {
      if let image = maybeImage {
        Image(uiImage: image)
          .resizable()
          .scaledToFill()
          .frame(width: size, height: size)
          .clipShape(RoundedRectangle(cornerRadius: size / 4, style: .continuous))
          .accessibilityHidden(true)
      }
    }
    .task(id: loadKey) {
      guard let version = maybeVersion else {
        maybeImage = nil
        isWaitingForRetry = false
        await model.forgetSpacePicture(spaceId: spaceId)
        return
      }
      switch await model.spacePicture(spaceId: spaceId, version: version) {
      case .picture(let image):
        maybeImage = image
        isWaitingForRetry = false
      case .none:
        maybeImage = nil
        isWaitingForRetry = false
      case .later:
        isWaitingForRetry = true
      }
    }
    .onChange(of: maybeWaitingRound) { maybeBefore, maybeNow in
      // A round ran while this picture waited for it (not the start of the wait): ask again.
      if maybeBefore != nil, maybeNow != nil { attempt += 1 }
    }
  }

  /// The retry round, read only while this picture waits for it: pictures that are here aren't
  /// drawn again when another one fails.
  private var maybeWaitingRound: Int? {
    isWaitingForRetry ? model.pictureRetries.round : nil
  }

  private var loadKey: LoadKey {
    LoadKey(spaceId: spaceId, version: maybeVersion, isConnected: model.connection?.isConnected == true, attempt: attempt)
  }

  private struct LoadKey: Equatable {
    let spaceId: String
    let version: String?
    let isConnected: Bool
    let attempt: Int
  }
}

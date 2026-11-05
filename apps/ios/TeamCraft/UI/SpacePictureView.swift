import SwiftUI

/// A space's picture as a small rounded square, or nothing without one (so the row keeps its
/// current look), while it loads and when it can't be loaded. It is fetched only while on screen.
struct SpacePictureView: View {
  @Environment(AppModel.self) private var model
  let spaceId: String
  let maybeVersion: String?
  var size: CGFloat = 24

  @State private var maybeImage: UIImage?

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
    .task(id: LoadKey(spaceId: spaceId, version: maybeVersion, isConnected: model.connection?.isConnected == true)) {
      guard let version = maybeVersion else {
        maybeImage = nil
        await model.forgetSpacePicture(spaceId: spaceId)
        return
      }
      maybeImage = await model.spacePicture(spaceId: spaceId, version: version)
    }
  }

  private struct LoadKey: Equatable {
    let spaceId: String
    let version: String?
    let isConnected: Bool
  }
}

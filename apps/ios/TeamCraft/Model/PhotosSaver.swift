import Photos
import SwiftUI
import UniformTypeIdentifiers

/// Saves a downloaded picture or video to the user's Photos library (#2011). Only the add-only
/// permission is asked for: the app never reads the library.
enum PhotosSaver {
  enum Outcome: Equatable {
    case saved
    /// The user said no (or Photos is restricted): the Settings app is where that changes.
    case denied
    case failed
  }

  static func save(fileAt url: URL, mediaType: String) async -> Outcome {
    let status = await PHPhotoLibrary.requestAuthorization(for: .addOnly)
    guard status == .authorized || status == .limited else { return .denied }
    let isVideo = mediaType.hasPrefix("video/")
    let options = PHAssetResourceCreationOptions()
    options.uniformTypeIdentifier = UTType(mimeType: mediaType)?.identifier
    do {
      try await PHPhotoLibrary.shared().performChanges {
        PHAssetCreationRequest.forAsset().addResource(with: isVideo ? .video : .photo, fileURL: url, options: options)
      }
      return .saved
    } catch {
      return .failed
    }
  }
}

extension View {
  /// Says why saving to Photos did not work; "denied" offers the Settings app.
  func photosSaveAlert(_ outcome: Binding<PhotosSaver.Outcome?>) -> some View {
    alert(
      "picture_save_failed",
      isPresented: Binding(
        get: { outcome.wrappedValue == .denied || outcome.wrappedValue == .failed },
        set: { if !$0 { outcome.wrappedValue = nil } })
    ) {
      if outcome.wrappedValue == .denied {
        Button("picture_save_open_settings") {
          if let url = URL(string: UIApplication.openSettingsURLString) { UIApplication.shared.open(url) }
        }
      }
      Button("done", role: .cancel) {}
    } message: {
      Text(outcome.wrappedValue == .denied ? "picture_save_denied" : "picture_save_error")
    }
  }
}

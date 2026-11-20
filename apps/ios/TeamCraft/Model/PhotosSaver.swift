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

extension PhotosSaver {
  /// A downloaded picture, and for a Live Photo or Motion Photo its motion too (#2181): saved as a
  /// Live Photo when Photos takes the pair, else as the picture and the motion as a video. A motion
  /// that doesn't come down leaves the picture saved alone.
  static func save(picture file: FileInfo, at url: URL, model: AppModel) async -> Outcome {
    guard let motion = file.motion, file.isLive, let motionURL = await downloadedMotion(of: file, motion, model: model) else {
      return await save(fileAt: url, mediaType: file.mediaType)
    }
    let status = await PHPhotoLibrary.requestAuthorization(for: .addOnly)
    guard status == .authorized || status == .limited else { return .denied }
    let pictureOptions = PHAssetResourceCreationOptions()
    pictureOptions.uniformTypeIdentifier = UTType(mimeType: file.mediaType)?.identifier
    let videoOptions = PHAssetResourceCreationOptions()
    videoOptions.uniformTypeIdentifier = UTType(mimeType: motion.mediaType)?.identifier
    do {
      try await PHPhotoLibrary.shared().performChanges {
        let request = PHAssetCreationRequest.forAsset()
        request.addResource(with: .photo, fileURL: url, options: pictureOptions)
        request.addResource(with: .pairedVideo, fileURL: motionURL, options: videoOptions)
      }
      return .saved
    } catch {
      // Photos pairs only files that name each other (Apple's content identifier), which a picture
      // and clip from another phone may not: then both go in on their own.
      let picture = await save(fileAt: url, mediaType: file.mediaType)
      guard picture == .saved else { return picture }
      return await save(fileAt: motionURL, mediaType: motion.mediaType)
    }
  }

  /// The motion's decrypted file, downloaded first if needed; `nil` when it doesn't come.
  @MainActor
  private static func downloadedMotion(of file: FileInfo, _ motion: MotionInfo, model: AppModel) async -> URL? {
    if let path = motion.localPath { return URL(fileURLWithPath: path) }
    await model.perform(.downloadAttachment(conversationId: file.conversation, messageId: file.message, fileId: motion.fileId))
    // The download reports its end in the store; look again for up to a minute.
    for _ in 0..<120 {
      try? await Task.sleep(for: .milliseconds(500))
      let extras = await model.extras(conversationId: file.conversation, messageIds: [file.message])
      guard let current = extras[file.message]?.files?.first(where: { $0.fileId == file.fileId })?.motion else { continue }
      if let path = current.localPath { return URL(fileURLWithPath: path) }
      if current.transfer.state == "failed" { return nil }
    }
    return nil
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

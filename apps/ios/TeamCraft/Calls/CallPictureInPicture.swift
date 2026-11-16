import AVFoundation
import AVKit
import SwiftUI
import UIKit

/// Picture-in-picture for a video call (#2067): leaving the app while the call screen shows video
/// keeps the person talking in a floating window, as FaceTime does. The system's video-call content
/// source (`AVPictureInPictureVideoCallViewController`) shows a layer that gets the same frames as
/// the stage's tile (`RemoteVideoRenderer.setMirror`). Audio-only calls get none: the status bar's
/// call pill and the app's return bar lead back to them.
@MainActor
final class CallPictureInPicture: NSObject, AVPictureInPictureControllerDelegate {
  /// The call screen's view picture-in-picture grows out of and returns to.
  let sourceView = UIView()

  private let displayLayer = AVSampleBufferDisplayLayer()
  private let content = AVPictureInPictureVideoCallViewController()
  private var maybeController: AVPictureInPictureController?
  private var maybeRenderer: RemoteVideoRenderer?

  /// The video's shape in the floating window: portrait, as the phone holds a call.
  private static let contentSize = CGSize(width: 1080, height: 1440)

  override init() {
    super.init()
    displayLayer.videoGravity = .resizeAspectFill
    content.preferredContentSize = Self.contentSize
    let host = LayerHostView(hosting: displayLayer)
    host.frame = content.view.bounds
    host.autoresizingMask = [.flexibleWidth, .flexibleHeight]
    content.view.addSubview(host)
  }

  /// Shows `maybeRenderer`'s video when the app goes to the background; `nil` (no video): nothing
  /// starts by itself.
  func show(_ maybeRenderer: RemoteVideoRenderer?) {
    if self.maybeRenderer !== maybeRenderer {
      self.maybeRenderer?.setMirror(nil)
      maybeRenderer?.setMirror(displayLayer)
      self.maybeRenderer = maybeRenderer
    }
    if maybeRenderer != nil, maybeController == nil, AVPictureInPictureController.isPictureInPictureSupported() {
      let source = AVPictureInPictureController.ContentSource(
        activeVideoCallSourceView: sourceView, contentViewController: content)
      let controller = AVPictureInPictureController(contentSource: source)
      controller.delegate = self
      maybeController = controller
    }
    maybeController?.canStartPictureInPictureAutomaticallyFromInline = maybeRenderer != nil
  }

  /// The call ended or its screen went away.
  func stop() {
    maybeController?.stopPictureInPicture()
    maybeController = nil
    maybeRenderer?.setMirror(nil)
    maybeRenderer = nil
  }

  // MARK: AVPictureInPictureControllerDelegate

  nonisolated func pictureInPictureController(
    _ pictureInPictureController: AVPictureInPictureController,
    restoreUserInterfaceForPictureInPictureStopWithCompletionHandler completionHandler: @escaping (Bool) -> Void
  ) {
    // The call screen is still there behind it: the app only has to come back, which iOS does.
    completionHandler(true)
  }
}

/// Puts picture-in-picture's source view into the call screen (behind the stage).
struct PictureInPictureSource: UIViewRepresentable {
  let pictureInPicture: CallPictureInPicture

  func makeUIView(context: Context) -> UIView {
    pictureInPicture.sourceView
  }

  func updateUIView(_ view: UIView, context: Context) {}
}

/// Which video picture-in-picture shows (pure, for the tests).
enum CallPictureInPictureChoice {
  /// A shared screen first (what people look at), else the loudest speaker's camera, else the first
  /// video; only video the phone decodes.
  static func stream(
    among streams: [RemoteVideoStream], speaking: [String], userIdByDevice: [String: String]
  ) -> RemoteVideoStream? {
    let playable = streams.filter(\.isPlayable)
    if let screen = playable.first(where: \.isScreen) { return screen }
    for user in speaking {
      if let stream = playable.first(where: { userIdByDevice[$0.deviceId] == user }) { return stream }
    }
    return playable.first
  }
}

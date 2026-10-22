import AVFoundation
import Foundation
import SwiftUI

/// The camera as a call video track: `AVCaptureSession` frames go to the hardware H.264 encoder
/// and from there as Annex B access units into the core's call session, which encrypts and
/// forwards them (DAVE). The encoder is created from the first frame, so the published size is the
/// size the camera really delivers.
final class CameraStreamer: NSObject, AVCaptureVideoDataOutputSampleBufferDelegate, @unchecked Sendable {
  /// Bitrate for a portrait HD track: enough for a talking head, light on mobile data.
  private static let bitrate = 1_200_000
  private static let frameRate = 30

  let captureSession = AVCaptureSession()

  private let sessionQueue = DispatchQueue(label: "de.zeitgleich.teamcraft.camera-session")
  private let frameQueue = DispatchQueue(label: "de.zeitgleich.teamcraft.camera-frames")
  private let lock = NSLock()
  private var maybeCall: CallSession?
  private var maybeEncoder: H264Encoder?
  private var isPaused = false
  private var isConfigured = false
  private var position = AVCaptureDevice.Position.front

  /// Asks for the permission if needed; `false` if the user said no.
  static func requestAccess() async -> Bool {
    await AVCaptureDevice.requestAccess(for: .video)
  }

  /// Starts capturing into `call`'s camera track.
  func start(sending call: CallSession) {
    lock.lock()
    maybeCall = call
    isPaused = false
    lock.unlock()
    sessionQueue.async { [self] in
      configureIfNeeded()
      if !captureSession.isRunning { captureSession.startRunning() }
    }
  }

  /// Stops capturing and ends the camera track.
  func stop() {
    lock.lock()
    let call = maybeCall
    maybeCall = nil
    maybeEncoder = nil
    lock.unlock()
    call?.stopCamera()
    sessionQueue.async { [self] in
      if captureSession.isRunning { captureSession.stopRunning() }
    }
  }

  /// While a call is on hold nothing is sent.
  func setPaused(_ isPaused: Bool) {
    lock.lock()
    self.isPaused = isPaused
    lock.unlock()
  }

  /// A receiver needs a fresh keyframe.
  func requestKeyframe() {
    lock.lock()
    let encoder = maybeEncoder
    lock.unlock()
    encoder?.requestKeyframe()
  }

  /// Front or back camera.
  func flip() {
    sessionQueue.async { [self] in
      position = position == .front ? .back : .front
      captureSession.beginConfiguration()
      for input in captureSession.inputs { captureSession.removeInput(input) }
      addInput()
      captureSession.commitConfiguration()
    }
  }

  func captureOutput(
    _ output: AVCaptureOutput, didOutput sampleBuffer: CMSampleBuffer, from connection: AVCaptureConnection
  ) {
    guard let pixelBuffer = CMSampleBufferGetImageBuffer(sampleBuffer) else { return }
    lock.lock()
    let paused = isPaused
    let call = maybeCall
    var encoder = maybeEncoder
    lock.unlock()
    guard !paused, let call else { return }
    if encoder == nil {
      encoder = makeEncoder(for: pixelBuffer, call: call)
      lock.lock()
      maybeEncoder = encoder
      lock.unlock()
    }
    encoder?.encode(pixelBuffer, at: sampleBuffer.presentationTimeStamp)
  }

  // MARK: Private

  private func makeEncoder(for pixelBuffer: CVPixelBuffer, call: CallSession) -> H264Encoder? {
    let width = CVPixelBufferGetWidth(pixelBuffer)
    let height = CVPixelBufferGetHeight(pixelBuffer)
    guard
      let encoder = H264Encoder(
        width: width, height: height, bitrate: Self.bitrate, frameRate: Self.frameRate,
        output: { data, timestamp, isKeyframe in
          call.sendCameraFrame(data: data, timestamp: timestamp, isKeyframe: isKeyframe)
        })
    else { return nil }
    call.publishCamera(
      width: UInt16(clamping: width), height: UInt16(clamping: height), maxFramerate: UInt8(Self.frameRate),
      maxBitrateBps: UInt32(Self.bitrate))
    return encoder
  }

  private func configureIfNeeded() {
    guard !isConfigured else { return }
    isConfigured = true
    captureSession.beginConfiguration()
    captureSession.sessionPreset = captureSession.canSetSessionPreset(.hd1280x720) ? .hd1280x720 : .vga640x480
    addInput()
    let output = AVCaptureVideoDataOutput()
    output.alwaysDiscardsLateVideoFrames = true
    output.videoSettings = [
      kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange
    ]
    output.setSampleBufferDelegate(self, queue: frameQueue)
    if captureSession.canAddOutput(output) {
      captureSession.addOutput(output)
      // Portrait frames, rotated by the capture hardware.
      if let connection = output.connection(with: .video), connection.isVideoRotationAngleSupported(90) {
        connection.videoRotationAngle = 90
      }
    }
    captureSession.commitConfiguration()
  }

  private func addInput() {
    guard
      let device = AVCaptureDevice.default(.builtInWideAngleCamera, for: .video, position: position),
      let input = try? AVCaptureDeviceInput(device: device), captureSession.canAddInput(input)
    else { return }
    captureSession.addInput(input)
  }
}

/// The local camera picture, mirrored like a mirror for the front camera.
struct CameraPreviewView: UIViewRepresentable {
  let session: AVCaptureSession

  func makeUIView(context: Context) -> PreviewHostView {
    let view = PreviewHostView()
    view.previewLayer?.session = session
    view.previewLayer?.videoGravity = .resizeAspectFill
    return view
  }

  func updateUIView(_ view: PreviewHostView, context: Context) {}
}

final class PreviewHostView: UIView {
  override class var layerClass: AnyClass { AVCaptureVideoPreviewLayer.self }

  var previewLayer: AVCaptureVideoPreviewLayer? { layer as? AVCaptureVideoPreviewLayer }
}

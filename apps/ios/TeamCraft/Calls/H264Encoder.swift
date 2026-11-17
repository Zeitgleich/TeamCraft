import CoreMedia
import Foundation
import VideoToolbox

/// Hardware H.264 encoding of camera frames with VideoToolbox: Baseline profile without B-frames
/// (what the other clients decode), real-time, a keyframe every two seconds and on request. Output
/// is Annex B with SPS and PPS in front of every keyframe, the form the call engine forwards.
final class H264Encoder: @unchecked Sendable {
  /// Called on VideoToolbox's thread with one access unit, its RTP timestamp (90 kHz) and whether
  /// it is a keyframe.
  typealias Output = @Sendable (Data, UInt32, Bool) -> Void

  private let session: VTCompressionSession
  private let output: Output
  private let lock = NSLock()
  private var isKeyframeRequested = false

  /// `nil` if the system has no encoder for this size.
  init?(width: Int, height: Int, bitrate: Int, frameRate: Int, output: @escaping Output) {
    var created: VTCompressionSession?
    let status = VTCompressionSessionCreate(
      allocator: nil, width: Int32(width), height: Int32(height), codecType: kCMVideoCodecType_H264,
      encoderSpecification: nil, imageBufferAttributes: nil, compressedDataAllocator: nil,
      outputCallback: nil, refcon: nil, compressionSessionOut: &created)
    guard status == noErr, let created else { return nil }
    session = created
    self.output = output
    Self.set(created, kVTCompressionPropertyKey_RealTime, kCFBooleanTrue)
    Self.set(created, kVTCompressionPropertyKey_ProfileLevel, kVTProfileLevel_H264_Baseline_AutoLevel)
    Self.set(created, kVTCompressionPropertyKey_AllowFrameReordering, kCFBooleanFalse)
    Self.setBitrate(created, bitrate)
    Self.set(created, kVTCompressionPropertyKey_ExpectedFrameRate, NSNumber(value: frameRate))
    Self.set(created, kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration, NSNumber(value: 2))
    VTCompressionSessionPrepareToEncodeFrames(created)
  }

  deinit {
    VTCompressionSessionInvalidate(session)
  }

  /// Encodes at `bitrate` (bit/s) from the next frame on, without a new session or a keyframe: the
  /// call engine's share of the uplink for the camera (#1989).
  func setBitrate(_ bitrate: Int) {
    Self.setBitrate(session, bitrate)
  }

  /// The next frame becomes a keyframe (a receiver joined or lost a frame).
  func requestKeyframe() {
    lock.lock()
    isKeyframeRequested = true
    lock.unlock()
  }

  func encode(_ pixelBuffer: CVPixelBuffer, at time: CMTime) {
    lock.lock()
    let isForced = isKeyframeRequested
    isKeyframeRequested = false
    lock.unlock()
    let properties = isForced ? [kVTEncodeFrameOptionKey_ForceKeyFrame: true] as CFDictionary : nil
    let output = output
    VTCompressionSessionEncodeFrame(
      session, imageBuffer: pixelBuffer, presentationTimeStamp: time, duration: .invalid,
      frameProperties: properties, infoFlagsOut: nil
    ) { status, _, sampleBuffer in
      guard status == noErr, let sampleBuffer, let frame = Self.accessUnit(from: sampleBuffer) else { return }
      let ticks = Int64(sampleBuffer.presentationTimeStamp.seconds * 90_000)
      output(frame.data, UInt32(truncatingIfNeeded: ticks), frame.isKeyframe)
    }
  }

  // MARK: Private

  private static func set(_ session: VTCompressionSession, _ key: CFString, _ value: CFTypeRef) {
    VTSessionSetProperty(session, key: key, value: value)
  }

  /// The average bitrate, and a cap of one and a half times it per second: the average alone lets
  /// a moving picture overshoot for seconds, which is what a limited uplink can't take.
  private static func setBitrate(_ session: VTCompressionSession, _ bitrate: Int) {
    set(session, kVTCompressionPropertyKey_AverageBitRate, NSNumber(value: bitrate))
    let bytesPerSecond = bitrate * 3 / 2 / 8
    let limits = [NSNumber(value: bytesPerSecond), NSNumber(value: 1)] as CFArray
    set(session, kVTCompressionPropertyKey_DataRateLimits, limits)
  }

  /// The encoded sample as an Annex B access unit; keyframes carry SPS and PPS.
  private static func accessUnit(from sampleBuffer: CMSampleBuffer) -> (data: Data, isKeyframe: Bool)? {
    guard CMSampleBufferDataIsReady(sampleBuffer), let block = CMSampleBufferGetDataBuffer(sampleBuffer) else {
      return nil
    }
    var length = 0
    var pointer: UnsafeMutablePointer<CChar>?
    guard
      CMBlockBufferGetDataPointer(
        block, atOffset: 0, lengthAtOffsetOut: nil, totalLengthOut: &length, dataPointerOut: &pointer)
        == kCMBlockBufferNoErr, let pointer,
      let slices = AnnexB.fromAVCC(Data(bytes: pointer, count: length))
    else { return nil }
    guard isKeyframe(sampleBuffer) else { return (slices, false) }
    let sets = sampleBuffer.formatDescription.map(parameterSets) ?? []
    return (AnnexB.accessUnit(parameterSets: sets, slices: slices), true)
  }

  private static func isKeyframe(_ sampleBuffer: CMSampleBuffer) -> Bool {
    guard
      let attachments = CMSampleBufferGetSampleAttachmentsArray(sampleBuffer, createIfNecessary: false)
        as? [[CFString: Any]],
      let first = attachments.first
    else { return true }
    return !((first[kCMSampleAttachmentKey_NotSync] as? Bool) ?? false)
  }

  /// SPS and PPS of the stream, from the format description.
  private static func parameterSets(of format: CMFormatDescription) -> [Data] {
    var count = 0
    guard
      CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
        format, parameterSetIndex: 0, parameterSetPointerOut: nil, parameterSetSizeOut: nil,
        parameterSetCountOut: &count, nalUnitHeaderLengthOut: nil) == noErr
    else { return [] }
    var sets: [Data] = []
    for index in 0..<count {
      var pointer: UnsafePointer<UInt8>?
      var size = 0
      let status = CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
        format, parameterSetIndex: index, parameterSetPointerOut: &pointer, parameterSetSizeOut: &size,
        parameterSetCountOut: nil, nalUnitHeaderLengthOut: nil)
      if status == noErr, let pointer { sets.append(Data(bytes: pointer, count: size)) }
    }
    return sets
  }
}

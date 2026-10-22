import AVFoundation
import CoreMedia
import Foundation
import SwiftUI

/// Shows one remote video track: Annex B access units from the call engine become AVCC samples
/// with a format description from the stream's SPS and PPS, and `AVSampleBufferDisplayLayer`
/// decodes and displays them (hardware H.264 decoder, no VTDecompressionSession needed).
final class RemoteVideoRenderer: @unchecked Sendable {
  let layer = AVSampleBufferDisplayLayer()

  private let queue = DispatchQueue(label: "de.zeitgleich.teamcraft.video-render")
  private var maybeFormat: CMVideoFormatDescription?
  private var maybeSequenceSet: Data?
  private var maybePictureSet: Data?

  init() {
    layer.videoGravity = .resizeAspect
  }

  /// Decodes and shows one frame. Until the first keyframe (with parameter sets) arrives, frames
  /// are skipped.
  func push(data: Data, timestamp: UInt32) {
    queue.async { [self] in
      var slices: [Data] = []
      var sequenceSet: Data?
      var pictureSet: Data?
      for unit in AnnexB.nalUnits(in: data) {
        switch AnnexB.nalType(unit) {
        case AnnexB.NalType.sequenceParameterSet: sequenceSet = unit
        case AnnexB.NalType.pictureParameterSet: pictureSet = unit
        case AnnexB.NalType.accessUnitDelimiter: break
        default: slices.append(unit)
        }
      }
      if let sequenceSet, let pictureSet, sequenceSet != maybeSequenceSet || pictureSet != maybePictureSet {
        maybeFormat = Self.makeFormat(sequenceSet: sequenceSet, pictureSet: pictureSet)
        maybeSequenceSet = sequenceSet
        maybePictureSet = pictureSet
      }
      guard let format = maybeFormat, !slices.isEmpty,
        let sample = Self.makeSample(AnnexB.toAVCC(slices), format: format, timestamp: timestamp)
      else { return }
      if layer.status == .failed { layer.flush() }
      layer.enqueue(sample)
    }
  }

  /// Drops what is shown (the track ended).
  func reset() {
    queue.async { [self] in
      layer.flushAndRemoveImage()
      maybeFormat = nil
      maybeSequenceSet = nil
      maybePictureSet = nil
    }
  }

  // MARK: Private

  private static func makeFormat(sequenceSet: Data, pictureSet: Data) -> CMVideoFormatDescription? {
    var format: CMVideoFormatDescription?
    let status = sequenceSet.withUnsafeBytes { sequenceBytes in
      pictureSet.withUnsafeBytes { pictureBytes -> OSStatus in
        guard let sequencePointer = sequenceBytes.bindMemory(to: UInt8.self).baseAddress,
          let picturePointer = pictureBytes.bindMemory(to: UInt8.self).baseAddress
        else { return -1 }
        let pointers = [sequencePointer, picturePointer]
        let sizes = [sequenceSet.count, pictureSet.count]
        return CMVideoFormatDescriptionCreateFromH264ParameterSets(
          allocator: kCFAllocatorDefault, parameterSetCount: 2, parameterSetPointers: pointers,
          parameterSetSizes: sizes, nalUnitHeaderLength: 4, formatDescriptionOut: &format)
      }
    }
    return status == noErr ? format : nil
  }

  /// A sample for `layer` that is displayed right away (no reordering, no jitter buffer here).
  private static func makeSample(_ avcc: Data, format: CMVideoFormatDescription, timestamp: UInt32)
    -> CMSampleBuffer?
  {
    var maybeBlock: CMBlockBuffer?
    let size = avcc.count
    guard
      CMBlockBufferCreateWithMemoryBlock(
        allocator: kCFAllocatorDefault, memoryBlock: nil, blockLength: size, blockAllocator: kCFAllocatorDefault,
        customBlockSource: nil, offsetToData: 0, dataLength: size, flags: 0, blockBufferOut: &maybeBlock)
        == kCMBlockBufferNoErr, let block = maybeBlock
    else { return nil }
    let copied = avcc.withUnsafeBytes { bytes -> OSStatus in
      guard let base = bytes.baseAddress else { return -1 }
      return CMBlockBufferReplaceDataBytes(
        with: base, blockBuffer: block, offsetIntoDestination: 0, dataLength: size)
    }
    guard copied == kCMBlockBufferNoErr else { return nil }
    var timing = CMSampleTimingInfo(
      duration: .invalid, presentationTimeStamp: CMTime(value: CMTimeValue(timestamp), timescale: 90_000),
      decodeTimeStamp: .invalid)
    var sampleSize = size
    var maybeSample: CMSampleBuffer?
    guard
      CMSampleBufferCreateReady(
        allocator: kCFAllocatorDefault, dataBuffer: block, formatDescription: format, sampleCount: 1,
        sampleTimingEntryCount: 1, sampleTimingArray: &timing, sampleSizeEntryCount: 1,
        sampleSizeArray: &sampleSize, sampleBufferOut: &maybeSample) == noErr, let sample = maybeSample
    else { return nil }
    if let attachments = CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: true),
      CFArrayGetCount(attachments) > 0
    {
      let dictionary = unsafeBitCast(CFArrayGetValueAtIndex(attachments, 0), to: CFMutableDictionary.self)
      CFDictionarySetValue(
        dictionary, Unmanaged.passUnretained(kCMSampleAttachmentKey_DisplayImmediately).toOpaque(),
        Unmanaged.passUnretained(kCFBooleanTrue).toOpaque())
    }
    return sample
  }
}

/// Hosts a remote track's display layer.
struct RemoteVideoView: UIViewRepresentable {
  let renderer: RemoteVideoRenderer

  func makeUIView(context: Context) -> LayerHostView {
    LayerHostView(hosting: renderer.layer)
  }

  func updateUIView(_ view: LayerHostView, context: Context) {}
}

final class LayerHostView: UIView {
  private let hosted: CALayer

  init(hosting layer: CALayer) {
    hosted = layer
    super.init(frame: .zero)
    backgroundColor = .black
    self.layer.addSublayer(layer)
  }

  @available(*, unavailable)
  required init?(coder: NSCoder) {
    nil
  }

  override func layoutSubviews() {
    super.layoutSubviews()
    hosted.frame = bounds
  }
}

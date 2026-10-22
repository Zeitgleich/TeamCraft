import Foundation

/// H.264 byte stream helpers. The call engine carries access units in Annex B form (start codes,
/// SPS and PPS before every keyframe); VideoToolbox and `AVSampleBufferDisplayLayer` work with
/// AVCC (4-byte length prefixes) and out-of-band parameter sets. Pure functions, unit-tested.
enum AnnexB {
  static let startCode: [UInt8] = [0, 0, 0, 1]

  /// NAL unit types the call code cares about.
  enum NalType {
    static let sequenceParameterSet: UInt8 = 7
    static let pictureParameterSet: UInt8 = 8
    static let accessUnitDelimiter: UInt8 = 9
  }

  /// The NAL units of an Annex B stream, without their start codes (3- or 4-byte).
  static func nalUnits(in data: Data) -> [Data] {
    let bytes = [UInt8](data)
    var starts: [(payload: Int, code: Int)] = []
    var index = 0
    while index + 2 < bytes.count {
      if bytes[index] == 0, bytes[index + 1] == 0, bytes[index + 2] == 1 {
        starts.append((index + 3, index))
        index += 3
      } else {
        index += 1
      }
    }
    var units: [Data] = []
    for (position, start) in starts.enumerated() {
      var end = position + 1 < starts.count ? starts[position + 1].code : bytes.count
      // Zero bytes before the next start code belong to it (4-byte codes, trailing zeros).
      while end > start.payload, bytes[end - 1] == 0 { end -= 1 }
      if end > start.payload { units.append(Data(bytes[start.payload..<end])) }
    }
    return units
  }

  /// `nal_unit_type` of a NAL unit.
  static func nalType(_ unit: Data) -> UInt8 {
    (unit.first ?? 0) & 0x1F
  }

  /// An AVCC buffer (4-byte big-endian lengths) as Annex B; `nil` if the lengths don't fit.
  static func fromAVCC(_ data: Data) -> Data? {
    let bytes = [UInt8](data)
    var result = Data()
    var index = 0
    while index < bytes.count {
      guard index + 4 <= bytes.count else { return nil }
      let length =
        Int(bytes[index]) << 24 | Int(bytes[index + 1]) << 16 | Int(bytes[index + 2]) << 8 | Int(bytes[index + 3])
      index += 4
      guard length > 0, index + length <= bytes.count else { return nil }
      result.append(contentsOf: startCode)
      result.append(contentsOf: bytes[index..<index + length])
      index += length
    }
    return result
  }

  /// NAL units as an AVCC buffer (4-byte big-endian lengths).
  static func toAVCC(_ units: [Data]) -> Data {
    var result = Data()
    for unit in units {
      let length = UInt32(truncatingIfNeeded: unit.count)
      result.append(contentsOf: [
        UInt8(truncatingIfNeeded: length >> 24), UInt8(truncatingIfNeeded: length >> 16),
        UInt8(truncatingIfNeeded: length >> 8), UInt8(truncatingIfNeeded: length),
      ])
      result.append(unit)
    }
    return result
  }

  /// An Annex B access unit from parameter sets and slices.
  static func accessUnit(parameterSets: [Data], slices: Data) -> Data {
    var result = Data()
    for set in parameterSets {
      result.append(contentsOf: startCode)
      result.append(set)
    }
    result.append(slices)
    return result
  }
}

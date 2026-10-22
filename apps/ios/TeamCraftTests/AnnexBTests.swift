import XCTest

@testable import TeamCraft

final class AnnexBTests: XCTestCase {
  private let sps = Data([0x67, 0x42, 0x00, 0x1F])
  private let pps = Data([0x68, 0xCE, 0x3C, 0x80])
  private let idr = Data([0x65, 0x88, 0x84, 0x00, 0x10])

  func testSplitsThreeAndFourByteStartCodes() {
    var stream = Data([0, 0, 0, 1])
    stream.append(sps)
    stream.append(contentsOf: [0, 0, 1])
    stream.append(pps)
    stream.append(contentsOf: [0, 0, 0, 1])
    stream.append(idr)
    XCTAssertEqual(AnnexB.nalUnits(in: stream), [sps, pps, idr])
  }

  func testNalTypeIsTheLowFiveBits() {
    XCTAssertEqual(AnnexB.nalType(sps), AnnexB.NalType.sequenceParameterSet)
    XCTAssertEqual(AnnexB.nalType(pps), AnnexB.NalType.pictureParameterSet)
    XCTAssertEqual(AnnexB.nalType(idr), 5)
    XCTAssertEqual(AnnexB.nalType(Data()), 0)
  }

  func testAVCCRoundTrip() throws {
    let avcc = AnnexB.toAVCC([sps, idr])
    XCTAssertEqual(avcc.prefix(4), Data([0, 0, 0, 4]))
    let annexB = try XCTUnwrap(AnnexB.fromAVCC(avcc))
    XCTAssertEqual(AnnexB.nalUnits(in: annexB), [sps, idr])
  }

  func testMalformedAVCCIsRefused() {
    XCTAssertNil(AnnexB.fromAVCC(Data([0, 0, 0, 9, 1, 2])))
    XCTAssertNil(AnnexB.fromAVCC(Data([0, 0])))
    XCTAssertNil(AnnexB.fromAVCC(Data([0, 0, 0, 0])))
    XCTAssertEqual(AnnexB.fromAVCC(Data()), Data())
  }

  func testKeyframeAccessUnitStartsWithParameterSets() throws {
    let slices = try XCTUnwrap(AnnexB.fromAVCC(AnnexB.toAVCC([idr])))
    let unit = AnnexB.accessUnit(parameterSets: [sps, pps], slices: slices)
    XCTAssertEqual(AnnexB.nalUnits(in: unit).map(AnnexB.nalType), [7, 8, 5])
  }

  func testGarbageWithoutStartCodeHasNoUnits() {
    XCTAssertTrue(AnnexB.nalUnits(in: Data([1, 2, 3, 4])).isEmpty)
    XCTAssertTrue(AnnexB.nalUnits(in: Data()).isEmpty)
  }
}

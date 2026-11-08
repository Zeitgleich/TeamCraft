import XCTest

@testable import TeamCraft

final class HolidayRegionTests: XCTestCase {
  func testTheRegionIsAbsentWhileTheWorkspacesApplies() {
    let own = Wire.decode(
      HolidayRegionState.self,
      from: #"{"region":"DE-BY","instance_region":"DE","has_instance_feed":true,"newer":1}"#)
    XCTAssertEqual(own, HolidayRegionState(region: "DE-BY", instanceRegion: "DE", hasInstanceFeed: true))
    let instance = Wire.decode(HolidayRegionState.self, from: #"{"instance_region":"CH-ZH"}"#)
    XCTAssertEqual(instance, HolidayRegionState(region: nil, instanceRegion: "CH-ZH", hasInstanceFeed: false))
    XCTAssertEqual(
      Wire.decode(HolidayRegionState.self, from: "{}"),
      HolidayRegionState(region: nil, instanceRegion: "", hasInstanceFeed: false))
  }

  func testGoingBackToTheWorkspacesRegionSendsNoRegion() {
    XCTAssertEqual(HolidayRegions.requestBody(region: "CH-ZH"), #"{"region":"CH-ZH"}"#)
    XCTAssertEqual(HolidayRegions.requestBody(region: nil), "{}")
    XCTAssertEqual(HolidayRegions.requestBody(region: ""), "{}")
  }

  func testTheRegionsAreTheServersAndTheirKeysAreDistinct() {
    XCTAssertEqual(HolidayRegions.codes.count, 45)
    XCTAssertEqual(Set(HolidayRegions.codes).count, 45)
    XCTAssertEqual(HolidayRegions.titleKey(for: "DE-BY"), "holiday_region_de_by")
    XCTAssertEqual(HolidayRegions.titleKey(for: "AT"), "holiday_region_at")
    XCTAssertEqual(HolidayRegions.titleKey(for: ""), "holiday_region_none")
    XCTAssertEqual(Set(HolidayRegions.codes.map(HolidayRegions.titleKey)).count, 45)
  }
}

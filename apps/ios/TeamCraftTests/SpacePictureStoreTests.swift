import XCTest

@testable import TeamCraft

final class SpacePictureStoreTests: XCTestCase {
  private var directory: URL!

  override func setUp() {
    directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
  }

  override func tearDown() {
    try? FileManager.default.removeItem(at: directory)
  }

  func testANewVersionReplacesTheOldOne() async {
    let store = SpacePictureStore(directory: directory)
    await store.save(Data([1]), spaceId: "7", version: "a")
    await store.save(Data([2]), spaceId: "7", version: "b")
    let old = await store.data(spaceId: "7", version: "a")
    let current = await store.data(spaceId: "7", version: "b")
    XCTAssertNil(old)
    XCTAssertEqual(current, Data([2]))
  }

  func testARemovedPictureLeavesNoFileAndOtherSpacesStay() async {
    let store = SpacePictureStore(directory: directory)
    await store.save(Data([1]), spaceId: "7", version: "a")
    await store.save(Data([3]), spaceId: "8", version: "a")
    await store.remove(spaceId: "7")
    let removed = await store.data(spaceId: "7", version: "a")
    let kept = await store.data(spaceId: "8", version: "a")
    XCTAssertNil(removed)
    XCTAssertEqual(kept, Data([3]))
  }

  func testTheFolderStaysBounded() async {
    let store = SpacePictureStore(directory: directory)
    for id in 1...(SpacePictureStore.maxFiles + 5) {
      await store.save(Data([1]), spaceId: String(id), version: "a")
    }
    let files = (try? FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil)) ?? []
    XCTAssertEqual(files.count, SpacePictureStore.maxFiles)
  }

  func testAnIdThatIsNotNumericNamesNoFile() async {
    let store = SpacePictureStore(directory: directory)
    await store.save(Data([1]), spaceId: "../x", version: "a")
    let found = await store.data(spaceId: "../x", version: "a")
    XCTAssertNil(found)
  }
}

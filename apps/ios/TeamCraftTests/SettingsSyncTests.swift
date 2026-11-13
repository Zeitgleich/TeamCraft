import XCTest

@testable import TeamCraft

/// A stand-in for the app's stores: a value of the synced settings and the state string.
@MainActor
private final class FakeStores: SettingsSyncStores {
  var settings = FakeStores.defaults
  var state = ""
  private(set) var applied: [SyncedAppSettings] = []
  /// Tells the syncer about a write the way `UserDefaults` does.
  var onWrite: (() -> Void)?

  static let defaults = SyncedAppSettings(
    listLayout: .spaces, listGrouping: .mixed, favoritesSort: .alphabetical, animatedImages: .autoplay, sharesTyping: true,
    firstNameIsMention: true, channelMentionsNotify: true, keywords: [], confirmChannelOver: 10,
    callStatusSounds: .all, colorTheme: "brand", font: "system")

  func current() async -> SyncedAppSettings { settings }

  func apply(_ new: SyncedAppSettings, over before: SyncedAppSettings) async {
    applied.append(new)
    settings = new
    onWrite?()
  }
}

/// A stand-in for the core: what the account holds, and how often it was asked.
@MainActor
private final class FakeCore: SettingsSyncCore {
  var mark: UInt64 = 1
  var account: SyncedAppSettings?
  private(set) var syncCalls: [(local: SyncedAppSettings, state: String)] = []

  func settingsMark() async throws -> UInt64 { mark }

  func syncSettings(local: SyncedAppSettings, state: String) async throws -> SettingsSyncOutcome {
    syncCalls.append((local, state))
    return SettingsSyncOutcome(settings: account ?? local, state: "state-\(syncCalls.count)")
  }
}

@MainActor
final class SettingsSyncerTests: XCTestCase {
  private let stores = FakeStores()
  private let core = FakeCore()

  private func makeSyncer(write: Duration = .milliseconds(60), mark: Duration = .milliseconds(10)) -> SettingsSyncer {
    SettingsSyncer(stores: stores, openCore: { [core] in core }, writeDelay: write, markDelay: mark)
  }

  private func settle(_ milliseconds: Int = 250) async throws {
    try await Task.sleep(for: .milliseconds(milliseconds))
  }

  func testTheDebounceIsTheOneAndAHalfSecondsTheIssueAsks() {
    XCTAssertEqual(SettingsSyncer.writeDebounce, .milliseconds(1500))
  }

  func testSyncsAtStartAndStoresTheStateNextToTheSettings() async throws {
    let syncer = makeSyncer()
    syncer.start()
    try await settle()
    XCTAssertEqual(core.syncCalls.count, 1)
    XCTAssertEqual(core.syncCalls.first?.state, "")
    XCTAssertEqual(stores.state, "state-1")
    // The next run hands the stored state back.
    await syncer.sync()
    XCTAssertEqual(core.syncCalls.last?.state, "state-1")
  }

  func testARowOfChangesIsOneUpload() async throws {
    let syncer = makeSyncer()
    stores.settings.sharesTyping = false
    syncer.localMayHaveChanged()
    try await Task.sleep(for: .milliseconds(20))
    stores.settings.listLayout = .list
    syncer.localMayHaveChanged()
    try await Task.sleep(for: .milliseconds(20))
    stores.settings.keywords = ["krebs"]
    syncer.localMayHaveChanged()
    try await settle(400)
    XCTAssertEqual(core.syncCalls.count, 1)
    XCTAssertEqual(core.syncCalls.first?.local.sharesTyping, false)
    XCTAssertEqual(core.syncCalls.first?.local.listLayout, .list)
    XCTAssertEqual(core.syncCalls.first?.local.keywords, ["krebs"])
  }

  func testAColourThemeChoiceIsUploaded() async throws {
    let syncer = makeSyncer()
    stores.settings.colorTheme = "preset:pine"
    syncer.localMayHaveChanged()
    try await settle(400)
    XCTAssertEqual(core.syncCalls.count, 1)
    XCTAssertEqual(core.syncCalls.first?.local.colorTheme, "preset:pine")
  }

  func testNothingUploadsBeforeTheRest() async throws {
    let syncer = makeSyncer(write: .milliseconds(300))
    stores.settings.sharesTyping = false
    syncer.localMayHaveChanged()
    try await settle(100)
    XCTAssertTrue(core.syncCalls.isEmpty)
  }

  func testWritesFromTheAccountAreNotUserChanges() async throws {
    var theirs = FakeStores.defaults
    theirs.listLayout = .list
    theirs.animatedImages = .onTap
    core.account = theirs
    // Every store write notifies, as UserDefaults does.
    let syncer = makeSyncer()
    stores.onWrite = { syncer.localMayHaveChanged() }
    syncer.start()
    try await settle(500)
    XCTAssertEqual(stores.applied, [theirs])
    XCTAssertEqual(core.syncCalls.count, 1, "the applied values must not start another upload")
  }

  func testAnUnchangedNotificationIsNotAChange() async throws {
    let syncer = makeSyncer()
    syncer.start()
    try await settle()
    let before = core.syncCalls.count
    // Unrelated UserDefaults noise: the values are the same.
    syncer.localMayHaveChanged()
    syncer.localMayHaveChanged()
    try await settle(300)
    XCTAssertEqual(core.syncCalls.count, before)
  }

  func testTheMarkMovingSyncsAndAnUnmovedMarkDoesNot() async throws {
    let syncer = makeSyncer()
    syncer.start()
    try await settle()
    XCTAssertEqual(core.syncCalls.count, 1)
    syncer.storeDidChange()
    try await settle()
    XCTAssertEqual(core.syncCalls.count, 1, "same mark, nothing to do")
    core.mark = 2
    syncer.storeDidChange()
    syncer.storeDidChange()
    try await settle()
    XCTAssertEqual(core.syncCalls.count, 2)
  }

  func testNothingRunsWithoutACore() async throws {
    let syncer = SettingsSyncer(stores: stores, openCore: { nil }, writeDelay: .milliseconds(10), markDelay: .milliseconds(10))
    syncer.start()
    stores.settings.sharesTyping = false
    syncer.localMayHaveChanged()
    syncer.storeDidChange()
    try await settle()
    XCTAssertTrue(core.syncCalls.isEmpty)
    XCTAssertEqual(stores.state, "")
  }
}

@MainActor
final class AppSettingsStoresTests: XCTestCase {
  private var defaults: UserDefaults!
  private var mentions = MentionSettings(isFirstNameMention: true, doChannelMentionsNotify: true, keywords: [])
  private var mentionWrites = 0

  override func setUp() {
    defaults = UserDefaults(suiteName: "settings-sync-tests-\(UUID().uuidString)")
    mentions = AppSettingsStores.defaultMentions
    mentionWrites = 0
  }

  private func makeStores() -> AppSettingsStores {
    AppSettingsStores(
      defaults: defaults,
      readMentions: { [unowned self] in mentions },
      writeMentions: { [unowned self] new in
        mentions = new
        mentionWrites += 1
      })
  }

  func testAnEmptyDeviceHoldsTheRecordsDefaults() async {
    let current = await makeStores().current()
    XCTAssertEqual(current, FakeStores.defaults)
  }

  func testReadsTheStoresValues() async {
    defaults.set("list", forKey: ChatListLayout.key)
    defaults.set("bySpace", forKey: ChatListGrouping.key)
    defaults.set("on_tap", forKey: AnimatedImagesSetting.key)
    defaults.set(false, forKey: "share_typing")
    defaults.set("own_only", forKey: CallStatusSoundsSetting.key)
    mentions = MentionSettings(isFirstNameMention: false, doChannelMentionsNotify: false, keywords: ["a", "b"])
    let current = await makeStores().current()
    XCTAssertEqual(current.listLayout, .list)
    XCTAssertEqual(current.listGrouping, .bySpace)
    XCTAssertEqual(current.animatedImages, .onTap)
    XCTAssertFalse(current.sharesTyping)
    XCTAssertEqual(current.callStatusSounds, .ownOnly)
    XCTAssertFalse(current.firstNameIsMention)
    XCTAssertFalse(current.channelMentionsNotify)
    XCTAssertEqual(current.keywords, ["a", "b"])
  }

  func testWritesBackOnlyWhatDiffers() async {
    // A value no mapping would write back: it survives if the store is left alone.
    defaults.set("sentinel", forKey: ChatListLayout.key)
    defaults.set("sentinel", forKey: AnimatedImagesSetting.key)
    let stores = makeStores()
    let before = await stores.current()
    var new = before
    new.listGrouping = .bySpace
    new.sharesTyping = false
    await stores.apply(new, over: before)
    XCTAssertEqual(defaults.string(forKey: ChatListLayout.key), "sentinel")
    XCTAssertEqual(defaults.string(forKey: AnimatedImagesSetting.key), "sentinel")
    XCTAssertEqual(defaults.string(forKey: ChatListGrouping.key), "bySpace")
    XCTAssertEqual(defaults.object(forKey: "share_typing") as? Bool, false)
    XCTAssertNil(defaults.object(forKey: CallStatusSoundsSetting.key))
    XCTAssertEqual(mentionWrites, 0, "the mention rules didn't differ")
  }

  func testWritesTheMentionRulesOnceWhenAnyOfThemDiffers() async {
    let stores = makeStores()
    let before = await stores.current()
    var new = before
    new.keywords = ["krebs"]
    new.channelMentionsNotify = false
    await stores.apply(new, over: before)
    XCTAssertEqual(mentionWrites, 1)
    XCTAssertEqual(mentions.keywords, ["krebs"])
    XCTAssertFalse(mentions.doChannelMentionsNotify)
    XCTAssertTrue(mentions.isFirstNameMention)
  }

  func testWhatIosHasNoSettingForIsCarriedNotOverwritten() async {
    let stores = makeStores()
    let before = await stores.current()
    var new = before
    new.confirmChannelOver = 25
    new.font = "inter"
    await stores.apply(new, over: before)
    let after = await stores.current()
    XCTAssertEqual(after.confirmChannelOver, 25)
    XCTAssertEqual(after.font, "inter")
  }

  func testTheStateIsKeptAcrossInstances() {
    makeStores().state = "opaque"
    XCTAssertEqual(makeStores().state, "opaque")
  }

  func testEveryNameRoundTrips() {
    for layout in [AppListLayout.spaces, .list] {
      XCTAssertEqual(AppSettingsStores.listLayout(AppSettingsStores.raw(layout)), layout)
    }
    for sort in [AppFavoritesSort.alphabetical, .recent] {
      XCTAssertEqual(AppSettingsStores.favoritesSort(AppSettingsStores.raw(sort)), sort)
    }
    for grouping in [AppListGrouping.mixed, .bySpace] {
      XCTAssertEqual(AppSettingsStores.listGrouping(AppSettingsStores.raw(grouping)), grouping)
    }
    for images in [AppAnimatedImages.autoplay, .onTap] {
      XCTAssertEqual(AppSettingsStores.animatedImages(AppSettingsStores.raw(images)), images)
    }
    for sounds in [CallStatusSounds.all, .ownOnly, .off] {
      XCTAssertEqual(CallStatusSoundsSetting.stored(CallStatusSoundsSetting.raw(sounds)).sounds, sounds)
    }
  }
}

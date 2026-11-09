import Foundation
import OSLog

/// Keeps the settings that follow the account in step with it (#1483, e2ee.md §6.4): the chat list's
/// layout and grouping, animated images, typing, the mention rules and the call status sounds. The
/// rules and the mapping onto the account's keys live in the core (`syncSettings`); this hands over
/// the values of the app's stores and writes back what came out differently, as Android's
/// `SettingsSync` does.
///
/// It runs when the core opens, when the account's settings moved (another device's change arrives
/// through the core's change callback, no polling), and `writeDebounce` after the user changed one,
/// so a row of changes costs one upload. A change made offline stays in the store and goes out on
/// the next run.
///
/// Stays on the device, so not here: light/dark, send key, language, notification content, media
/// downloads, audio and video devices.

/// The core's side of the sync, so tests can stand in for it.
protocol SettingsSyncCore: AnyObject {
  /// Moves when the account's synced settings changed; 0 until the device merged the account once.
  func settingsMark() async throws -> UInt64
  func syncSettings(local: SyncedAppSettings, state: String) async throws -> SettingsSyncOutcome
}

extension TeamCraft: SettingsSyncCore {}

/// The app's own stores of the synced settings, so tests can stand in for them.
@MainActor
protocol SettingsSyncStores: AnyObject {
  /// What the stores hold now.
  func current() async -> SyncedAppSettings
  /// Writes the values of `settings` that differ from `before`, and only those.
  func apply(_ settings: SyncedAppSettings, over before: SyncedAppSettings) async
  /// The opaque state of the last sync, kept next to the settings (also across restarts).
  var state: String { get set }
}

@MainActor
final class SettingsSyncer {
  /// A row of changes in Settings goes to the account as one upload.
  nonisolated static let writeDebounce: Duration = .milliseconds(1500)
  /// The store reports a burst of changes during a catch-up; one look at the mark is enough.
  nonisolated static let markDebounce: Duration = .milliseconds(500)
  private static let log = Logger(subsystem: "de.zeitgleich.teamcraft", category: "settings-sync")

  private let stores: SettingsSyncStores
  private let openCore: () -> SettingsSyncCore?
  private let writeDelay: Duration
  private let markDelay: Duration

  /// What the stores held when the sync or the last noticed change looked: a store notification
  /// that finds the same values is not a change by the user (the sync's own writes land here).
  private var maybeLastSeen: SyncedAppSettings?
  /// The account's settings mark of the last run, and the core it was read from.
  private var maybeLastMark: UInt64?
  private var maybeLastCore: ObjectIdentifier?
  private var maybeWriteTask: Task<Void, Never>?
  private var maybeMarkTask: Task<Void, Never>?
  private var isSyncing = false
  private var needsAnother = false

  init(
    stores: SettingsSyncStores,
    openCore: @escaping () -> SettingsSyncCore?,
    writeDelay: Duration = SettingsSyncer.writeDebounce,
    markDelay: Duration = SettingsSyncer.markDebounce
  ) {
    self.stores = stores
    self.openCore = openCore
    self.writeDelay = writeDelay
    self.markDelay = markDelay
  }

  /// The core opened (or the app starts): look now.
  func start() {
    Task { [weak self] in await self?.sync() }
  }

  /// The core reported a change: sync if the account's settings moved (or the core is a new one).
  func storeDidChange() {
    maybeMarkTask?.cancel()
    maybeMarkTask = Task { [weak self] in
      guard let self else { return }
      try? await Task.sleep(for: self.markDelay)
      if Task.isCancelled { return }
      await self.syncIfAccountMoved()
    }
  }

  /// A store of these settings may have changed. A real change by the user (the values differ from
  /// what was last seen) schedules one upload after the rest; the sync's own writes don't.
  func localMayHaveChanged() {
    Task { [weak self] in
      guard let self, self.openCore() != nil else { return }
      let now = await self.stores.current()
      guard now != self.maybeLastSeen else { return }
      self.maybeLastSeen = now
      self.scheduleWrite()
    }
  }

  private func scheduleWrite() {
    maybeWriteTask?.cancel()
    maybeWriteTask = Task { [weak self] in
      guard let self else { return }
      try? await Task.sleep(for: self.writeDelay)
      if Task.isCancelled { return }
      await self.sync()
    }
  }

  private func syncIfAccountMoved() async {
    guard let core = openCore(), let mark = try? await core.settingsMark() else { return }
    if mark == maybeLastMark && ObjectIdentifier(core) == maybeLastCore { return }
    await sync()
  }

  /// One run at a time; a request that arrives during a run makes it go round once more.
  func sync() async {
    if isSyncing {
      needsAnother = true
      return
    }
    isSyncing = true
    defer { isSyncing = false }
    repeat {
      needsAnother = false
      await runOnce()
    } while needsAnother
  }

  private func runOnce() async {
    guard let core = openCore() else { return }
    let local = await stores.current()
    maybeLastSeen = local
    let outcome: SettingsSyncOutcome
    do {
      outcome = try await core.syncSettings(local: local, state: stores.state)
    } catch {
      // The values wait in the stores; the next change or account update tries again.
      Self.log.warning("settings not synced: \(String(describing: error), privacy: .public)")
      return
    }
    stores.state = outcome.state
    if outcome.settings != local {
      // Before the writes, so the stores' notifications for them find nothing new.
      maybeLastSeen = outcome.settings
      await stores.apply(outcome.settings, over: local)
    }
    maybeLastMark = try? await core.settingsMark()
    maybeLastCore = ObjectIdentifier(core)
  }
}

/// The settings in the app's stores: `UserDefaults` for the ones Settings → Chats keeps there, the
/// core for the mention rules. What iOS has no setting for yet (`@channel` confirmation count, colour
/// theme) is carried as the sync last returned it, so another device's value isn't overwritten.
@MainActor
final class AppSettingsStores: SettingsSyncStores {
  static let stateKey = "settings_sync_state"
  static let confirmChannelOverKey = "settings_sync_confirm_channel_over"
  static let colorThemeKey = "settings_sync_color_theme"
  /// The record's defaults for the settings iOS doesn't have.
  static let defaultConfirmChannelOver: UInt32 = 10
  static let defaultColorTheme = "brand"
  static let defaultMentions = MentionSettings(isFirstNameMention: true, doChannelMentionsNotify: true, keywords: [])

  private let defaults: UserDefaults
  private let readMentions: () async -> MentionSettings
  private let writeMentions: (MentionSettings) async -> Void

  init(
    defaults: UserDefaults = .standard,
    readMentions: @escaping () async -> MentionSettings,
    writeMentions: @escaping (MentionSettings) async -> Void
  ) {
    self.defaults = defaults
    self.readMentions = readMentions
    self.writeMentions = writeMentions
  }

  var state: String {
    get { defaults.string(forKey: Self.stateKey) ?? "" }
    set { defaults.set(newValue, forKey: Self.stateKey) }
  }

  func current() async -> SyncedAppSettings {
    let mentions = await readMentions()
    return SyncedAppSettings(
      listLayout: Self.listLayout(defaults.string(forKey: ChatListLayout.key)),
      listGrouping: Self.listGrouping(defaults.string(forKey: ChatListGrouping.key)),
      animatedImages: Self.animatedImages(defaults.string(forKey: AnimatedImagesSetting.key)),
      sharesTyping: defaults.object(forKey: Self.shareTypingKey) as? Bool ?? true,
      firstNameIsMention: mentions.isFirstNameMention,
      channelMentionsNotify: mentions.doChannelMentionsNotify,
      keywords: mentions.keywords,
      confirmChannelOver: defaults.object(forKey: Self.confirmChannelOverKey) == nil
        ? Self.defaultConfirmChannelOver : UInt32(clamping: defaults.integer(forKey: Self.confirmChannelOverKey)),
      callStatusSounds: CallStatusSoundsSetting.stored(defaults.string(forKey: CallStatusSoundsSetting.key)).sounds,
      colorTheme: defaults.string(forKey: Self.colorThemeKey) ?? Self.defaultColorTheme
    )
  }

  func apply(_ settings: SyncedAppSettings, over before: SyncedAppSettings) async {
    if settings.listLayout != before.listLayout {
      defaults.set(Self.raw(settings.listLayout), forKey: ChatListLayout.key)
    }
    if settings.listGrouping != before.listGrouping {
      defaults.set(Self.raw(settings.listGrouping), forKey: ChatListGrouping.key)
    }
    if settings.animatedImages != before.animatedImages {
      defaults.set(Self.raw(settings.animatedImages), forKey: AnimatedImagesSetting.key)
    }
    if settings.sharesTyping != before.sharesTyping {
      defaults.set(settings.sharesTyping, forKey: Self.shareTypingKey)
    }
    if settings.callStatusSounds != before.callStatusSounds {
      defaults.set(CallStatusSoundsSetting.raw(settings.callStatusSounds), forKey: CallStatusSoundsSetting.key)
    }
    if settings.confirmChannelOver != before.confirmChannelOver {
      defaults.set(settings.confirmChannelOver, forKey: Self.confirmChannelOverKey)
    }
    if settings.colorTheme != before.colorTheme {
      defaults.set(settings.colorTheme, forKey: Self.colorThemeKey)
    }
    if settings.firstNameIsMention != before.firstNameIsMention
      || settings.channelMentionsNotify != before.channelMentionsNotify
      || settings.keywords != before.keywords
    {
      await writeMentions(
        MentionSettings(
          isFirstNameMention: settings.firstNameIsMention,
          doChannelMentionsNotify: settings.channelMentionsNotify,
          keywords: settings.keywords))
    }
  }

  static let shareTypingKey = "share_typing"

  static func listLayout(_ maybeRaw: String?) -> AppListLayout {
    maybeRaw.flatMap(ChatListLayout.init(rawValue:)) == .list ? .list : .spaces
  }

  static func listGrouping(_ maybeRaw: String?) -> AppListGrouping {
    maybeRaw.flatMap(ChatListGrouping.init(rawValue:)) == .bySpace ? .bySpace : .mixed
  }

  static func animatedImages(_ maybeRaw: String?) -> AppAnimatedImages {
    AnimatedImagesSetting.stored(maybeRaw) == .onTap ? .onTap : .autoplay
  }

  static func raw(_ layout: AppListLayout) -> String {
    switch layout {
    case .spaces: return ChatListLayout.spaces.rawValue
    case .list: return ChatListLayout.list.rawValue
    }
  }

  static func raw(_ grouping: AppListGrouping) -> String {
    switch grouping {
    case .mixed: return ChatListGrouping.mixed.rawValue
    case .bySpace: return ChatListGrouping.bySpace.rawValue
    }
  }

  static func raw(_ images: AppAnimatedImages) -> String {
    switch images {
    case .autoplay: return AnimatedImagesSetting.autoplay.rawValue
    case .onTap: return AnimatedImagesSetting.onTap.rawValue
    }
  }
}

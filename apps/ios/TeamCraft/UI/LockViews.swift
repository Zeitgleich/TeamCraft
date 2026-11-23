import SwiftUI

/// Covers the app while it is locked; asks right away and again on the button.
struct LockView: View {
  let lock: AppLock

  var body: some View {
    VStack(spacing: Theme.spacing) {
      Image(systemName: "lock.fill").font(.app(.largeTitle)).foregroundStyle(Theme.accent)
      Text("app_lock_locked").font(.app(.headline))
      Button("app_lock_unlock") { Task { await lock.unlock(reason: String(localized: "app_lock_prompt")) } }
        .buttonStyle(PrimaryButtonStyle())
    }
    .padding()
    .frame(maxWidth: .infinity, maxHeight: .infinity)
    .background(Theme.background)
    .task { await lock.unlock(reason: String(localized: "app_lock_prompt")) }
  }
}

/// Hides the chats while the app is not active, so the app switcher shows no content.
struct PrivacyCover: View {
  /// Follows Dynamic Type with the text, as the server entry's symbols do.
  @ScaledMetric(relativeTo: .largeTitle) private var symbolSize: CGFloat = 48

  var body: some View {
    ZStack {
      Theme.background
      Image(systemName: "lock.shield").font(.system(size: symbolSize)).foregroundStyle(Theme.accent)
    }
    .ignoresSafeArea()
    .accessibilityHidden(true)
  }
}

/// The device section of the settings: app lock, its delay and the app switcher cover.
struct DeviceSecuritySection: View {
  @AppStorage(AppLock.enabledKey) private var isLockEnabled = false
  @AppStorage(AppLock.lockAfterKey) private var lockAfterRaw = AppLock.defaultLockAfter.rawValue
  @AppStorage(AppLock.privacyCoverKey) private var isPrivacyCoverOn = false

  var body: some View {
    Section {
      Toggle("app_lock", isOn: lockBinding).disabled(!AppLock.isAvailable && !isLockEnabled)
      if isLockEnabled {
        Picker("app_lock_after", selection: lockAfterBinding) {
          ForEach(LockAfter.allCases) { choice in Text(title(of: choice)).tag(choice.rawValue) }
        }
      }
      // With app lock on the cover is always used; the switch only matters without it.
      Toggle("privacy_cover", isOn: $isPrivacyCoverOn).disabled(isLockEnabled)
    } header: {
      Text("device_security_title")
    } footer: {
      if !AppLock.isAvailable { Text("app_lock_unavailable") }
    }
  }

  /// Switching the lock on is free; switching it off asks for Face ID / Touch ID or the passcode,
  /// so someone holding the unlocked phone can't remove it for later.
  private var lockBinding: Binding<Bool> {
    Binding(
      get: { isLockEnabled },
      set: { isOn in
        guard !isOn else {
          setLock(true)
          return
        }
        Task { @MainActor in
          if await AppLock.confirmChange(reason: String(localized: "app_lock_confirm_change")) {
            setLock(false)
          }
        }
      })
  }

  /// A shorter delay is free; a longer one asks, like switching the lock off.
  private var lockAfterBinding: Binding<String> {
    Binding(
      get: { lockAfterRaw },
      set: { raw in
        let current = LockAfter(rawValue: lockAfterRaw) ?? AppLock.defaultLockAfter
        let chosen = LockAfter(rawValue: raw) ?? AppLock.defaultLockAfter
        guard chosen.seconds > current.seconds else {
          lockAfterRaw = raw
          return
        }
        Task { @MainActor in
          if await AppLock.confirmChange(reason: String(localized: "app_lock_confirm_change")) {
            lockAfterRaw = raw
          }
        }
      })
  }

  private func setLock(_ isOn: Bool) {
    isLockEnabled = isOn
    // The notification extension follows it (generic notifications while the lock is on).
    SharedContainer().isAppLockOn = isOn
    // The home screen widget hides everything personal while the lock is on (#2076).
    AppModel.shared.homeWidget.storeDidChange(AppModel.shared)
    // So does the share extension's conversation list (#1037).
    AppModel.shared.shareTargetsUpdater.storeDidChange(AppModel.shared)
  }

  private func title(of choice: LockAfter) -> LocalizedStringKey {
    switch choice {
    case .immediately: return "app_lock_immediately"
    case .oneMinute: return "app_lock_one_minute"
    case .fiveMinutes: return "app_lock_five_minutes"
    case .fifteenMinutes: return "app_lock_fifteen_minutes"
    }
  }
}

/// The connection state along the top edge of the screen, below the status bar: a thin red bar while
/// the app can't reach the server (no network, the server down or restarting), yellow while it
/// connects, nothing once connected. A connection that comes back within the grace period never
/// shows it (`ConnectionGrace`). Messages wait in the outbox meanwhile.
struct ConnectionStripBar: View {
  static let height: CGFloat = 4
  /// A warm yellow that reads on light and dark backgrounds, as on Android.
  static let connectingYellow = Color(red: 0xF2 / 255, green: 0xB7 / 255, blue: 0x05 / 255)

  @Environment(AppModel.self) private var model
  private let network = NetworkMonitor.shared

  var body: some View {
    // A clock only while the connection is down: a connected app doesn't wake up every second.
    if ConnectionNotice.isDown(isOnline: network.isOnline, connection: model.connection) {
      TimelineView(.periodic(from: .now, by: 1)) { timeline in
        strip(at: timeline.date)
      }
    } else {
      strip(at: .now)
    }
  }

  private func strip(at now: Date) -> some View {
    let maybeNotice = ConnectionNotice.current(
      isOnline: network.isOnline, connection: model.connection, grace: model.connectionGrace, now: now)
    let maybeStrip = maybeNotice?.strip
    let color: Color =
      switch maybeStrip {
      case .down: Theme.danger
      case .connecting: Self.connectingYellow
      case nil: .clear
      }
    return Rectangle()
      .fill(color)
      .frame(maxWidth: .infinity)
      .frame(height: Self.height)
      .animation(Motion.fast, value: maybeStrip)
      .allowsHitTesting(false)
      .accessibilityElement()
      .accessibilityLabel(maybeNotice.flatMap { label(of: $0) } ?? Text(""))
      .accessibilityHidden(maybeStrip == nil)
  }

  private func label(of notice: ConnectionNotice) -> Text? {
    switch notice {
    case .deviceOffline: return Text("offline_banner")
    case .connecting: return Text("connection_connecting")
    case .unreachable(_, nil): return Text("connection_server_unreachable")
    case .unreachable, .slow, .signInRequired, .noRoleLeft: return nil
    }
  }
}

/// A banner with text and an action, only while the user must do something: sign in again, sign in
/// to the network (a captive portal) or sort out an untrusted certificate. Everything the app
/// handles by itself is the strip along the top (`ConnectionStripBar`).
struct OfflineBanner: View {
  @Environment(AppModel.self) private var model
  private let network = NetworkMonitor.shared

  var body: some View {
    // A clock only while the connection is down: a connected app doesn't wake up every second.
    if ConnectionNotice.isDown(isOnline: network.isOnline, connection: model.connection) {
      TimelineView(.periodic(from: .now, by: 1)) { timeline in
        bar(at: timeline.date)
      }
    } else {
      bar(at: .now)
    }
  }

  @ViewBuilder
  private func bar(at now: Date) -> some View {
    if let notice = ConnectionNotice.current(
      isOnline: network.isOnline, connection: model.connection, grace: model.connectionGrace, now: now),
      notice.needsBanner
    {
      HStack {
        text(of: notice)
          .font(.app(.footnote))
          .foregroundStyle(Theme.danger)
          .frame(maxWidth: .infinity, alignment: .leading)
        switch notice {
        case .unreachable:
          Button("connection_retry") { model.networkChanged() }
            .font(.app(.footnote).weight(.semibold))
        case .signInRequired, .noRoleLeft:
          Button("connection_sign_in_action") { Task { await model.signOut() } }
            .font(.app(.footnote).weight(.semibold))
        case .deviceOffline, .connecting, .slow:
          EmptyView()
        }
      }
      .padding(.horizontal).padding(.vertical, 6)
      .background(Theme.danger.opacity(0.12))
      .accessibilityElement(children: .combine)
    }
  }

  private func text(of notice: ConnectionNotice) -> Text {
    switch notice {
    case .unreachable(let seconds, let maybeInterception):
      switch maybeInterception {
      case .captivePortal: return Text("connection_captive_portal \(seconds)")
      case .untrustedCertificate: return Text("connection_untrusted_certificate \(seconds)")
      case nil: return Text("")
      }
    case .signInRequired:
      return model.isDeviceKeyLost ? Text("connection_device_key_lost") : Text("connection_sign_in")
    case .noRoleLeft:
      return Text("connection_no_role_left")
    // The strip's (`needsBanner`).
    case .deviceOffline, .connecting, .slow:
      return Text("")
    }
  }
}

/// A thin moving line under the banner while the app gets what happened while it was away: the
/// connection is being set up, or `isCatchingUp` (this conversation, or one in the list, still has
/// messages to come; issue #1457). Without it a chat looked complete for a moment and then
/// changed. It shows only after `SyncingLine.delay`, so a quick catch-up doesn't flash, and keeps
/// its height, so nothing below moves.
struct SyncingLine: View {
  static let delay: Duration = .milliseconds(300)
  static let height: CGFloat = 2

  let isCatchingUp: Bool
  @Environment(AppModel.self) private var model
  @Environment(\.accessibilityReduceMotion) private var reduceMotion
  @State private var isShown = false
  @State private var isSliding = false
  private let network = NetworkMonitor.shared

  private var isSyncing: Bool {
    ConnectionNotice.isSyncing(isOnline: network.isOnline, isCatchingUp: isCatchingUp, connection: model.connection)
  }

  var body: some View {
    Rectangle()
      .fill(Color.clear)
      .frame(height: Self.height)
      .overlay {
        if isShown {
          GeometryReader { proxy in
            Capsule()
              .fill(Color.accentColor)
              .frame(width: proxy.size.width * 0.35)
              // Reduce motion: a still bar instead of one sliding back and forth.
              .offset(x: reduceMotion ? proxy.size.width * 0.325 : (isSliding ? proxy.size.width * 0.65 : 0))
              .animation(
                reduceMotion ? nil : .easeInOut(duration: 1.1).repeatForever(autoreverses: true), value: isSliding)
          }
          .transition(.opacity)
        }
      }
      .clipped()
      .animation(Motion.fast, value: isShown)
      .accessibilityElement()
      .accessibilityLabel(Text("syncing_updates"))
      .accessibilityHidden(!isShown)
      .task(id: isSyncing) {
        if isSyncing {
          // Cancelled by the next change of `isSyncing`, so a short wait never shows the line.
          try? await Task.sleep(for: Self.delay)
          guard !Task.isCancelled else { return }
          isShown = true
          isSliding = true
        } else {
          isShown = false
          isSliding = false
        }
      }
  }
}

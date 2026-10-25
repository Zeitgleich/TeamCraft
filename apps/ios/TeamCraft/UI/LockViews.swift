import SwiftUI

/// Covers the app while it is locked; asks right away and again on the button.
struct LockView: View {
  let lock: AppLock

  var body: some View {
    VStack(spacing: Theme.spacing) {
      Image(systemName: "lock.fill").font(.largeTitle).foregroundStyle(Theme.accent)
      Text("app_lock_locked").font(.headline)
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
  var body: some View {
    ZStack {
      Theme.background
      Image(systemName: "lock.shield").font(.system(size: 48)).foregroundStyle(Theme.accent)
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

/// A thin bar while the app can't reach the server: the device is offline, the server is unreachable
/// although the device is online, or the session expired. Follows the core's connection state; new
/// messages wait, sent ones stay in the outbox.
struct OfflineBanner: View {
  @Environment(AppModel.self) private var model
  private let network = NetworkMonitor.shared
  @State private var maybeConnectingSince: Date?

  /// "Connecting" only shows when it takes longer than this; a quick reconnect needs no banner.
  private static let connectingGrace: TimeInterval = 3

  var body: some View {
    TimelineView(.periodic(from: .now, by: 1)) { timeline in
      if let notice = notice(at: timeline.date) {
        HStack {
          notice.text
            .font(.footnote)
            .foregroundStyle(Theme.danger)
            .frame(maxWidth: .infinity, alignment: .leading)
          if let action = notice.action {
            Button(action.label, action: action.run)
              .font(.footnote.weight(.semibold))
          }
        }
        .padding(.horizontal).padding(.vertical, 6)
        .background(Theme.danger.opacity(0.12))
      }
    }
    .onChange(of: model.connection, initial: true) { _, connection in
      if case .connecting = connection {
        if maybeConnectingSince == nil { maybeConnectingSince = .now }
      } else {
        maybeConnectingSince = nil
      }
    }
  }

  private struct Notice {
    let text: Text
    let action: Action?
  }

  private struct Action {
    let label: LocalizedStringKey
    let run: () -> Void
  }

  private func notice(at now: Date) -> Notice? {
    if !network.isOnline { return Notice(text: Text("offline_banner"), action: nil) }
    switch model.connection {
    case .offline(let retryAtMs, let maybeInterception):
      let seconds = max(0, Int((Double(retryAtMs) / 1000 - now.timeIntervalSince1970).rounded(.up)))
      let text: Text =
        switch maybeInterception {
        case .captivePortal: Text("connection_captive_portal \(seconds)")
        case .untrustedCertificate: Text("connection_untrusted_certificate \(seconds)")
        case nil: Text("connection_unreachable \(seconds)")
        }
      return Notice(
        text: text,
        action: Action(label: "connection_retry") { model.networkChanged() })
    case .signInRequired:
      return Notice(
        text: Text("connection_sign_in"),
        action: Action(label: "connection_sign_in_action") { Task { await model.signOut() } })
    case .connecting:
      guard let since = maybeConnectingSince, now.timeIntervalSince(since) >= Self.connectingGrace
      else {
        return nil
      }
      return Notice(text: Text("connection_connecting"), action: nil)
    case .connected, nil:
      return nil
    }
  }
}

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
  @AppStorage("share_typing") private var isSharingTyping = true

  var body: some View {
    Section {
      Toggle("app_lock", isOn: lockBinding).disabled(!AppLock.isAvailable && !isLockEnabled)
      if isLockEnabled {
        Picker("app_lock_after", selection: lockAfterBinding) {
          ForEach(LockAfter.allCases) { choice in Text(title(of: choice)).tag(choice.rawValue) }
        }
      }
      // With app lock on the cover is always used; the switch only matters without it.
      Toggle("privacy_cover", isOn: $isPrivacyCoverOn)
      Toggle("share_typing", isOn: $isSharingTyping).disabled(isLockEnabled)
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

/// A thin bar while the device is offline: new messages wait, sent ones stay in the outbox.
struct OfflineBanner: View {
  private let network = NetworkMonitor.shared

  var body: some View {
    if !network.isOnline {
      Text("offline_banner")
        .font(.footnote)
        .foregroundStyle(Theme.danger)
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal).padding(.vertical, 6)
        .background(Theme.danger.opacity(0.12))
    }
  }
}

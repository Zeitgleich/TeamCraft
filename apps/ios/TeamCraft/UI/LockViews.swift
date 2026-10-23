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
      Toggle("app_lock", isOn: $isLockEnabled).disabled(!AppLock.isAvailable && !isLockEnabled)
      if isLockEnabled {
        Picker("app_lock_after", selection: $lockAfterRaw) {
          ForEach(LockAfter.allCases) { choice in Text(title(of: choice)).tag(choice.rawValue) }
        }
      }
      Toggle("privacy_cover", isOn: $isPrivacyCoverOn)
    } header: {
      Text("device_security_title")
    } footer: {
      if !AppLock.isAvailable { Text("app_lock_unavailable") }
    }
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

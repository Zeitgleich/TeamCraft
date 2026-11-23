import SwiftUI

// Strict channels (e2ee.md §4.6.2, issue #1897), as desktop and web show them: the core decides
// what to show and which action to offer (`teamcraft-ffi`'s `strict_channel`); these views only
// render it. Nothing shows while the core returns no item, so the app claims no protection it
// doesn't give.

extension AppModel {
  /// A private channel's strict mode; `nil` while this app doesn't implement it, or no answer.
  func strictChannel(_ conversationId: String) async -> StrictChannelItem? {
    (try? await core().strictChannel(conversationId: conversationId)) ?? nil
  }

  /// The channel's members opened: its strict mode is read afresh, as on desktop and web.
  func refreshStrictChannel(_ conversationId: String) async {
    try? await core().refreshStrictChannel(conversationId: conversationId)
  }

  /// Runs a strict channel step; `false` after a failure, which the caller shows where the button
  /// was pressed.
  func strictStep(_ conversationId: String, _ step: StrictStepItem) async -> Bool {
    do {
      try await core().strictStep(conversationId: conversationId, step: step)
      return true
    } catch {
      return false
    }
  }
}

/// "Member changes" in a channel's members view.
struct StrictChannelSection: View {
  @Environment(AppModel.self) private var model
  let conversationId: String
  let strict: StrictChannelItem

  @State private var isRunning = false
  @State private var hasFailed = false
  @State private var maybeConfirming: StrictStepItem?

  var body: some View {
    Section {
      Label {
        Text(strict.isStrict ? "channel_strict_on" : "channel_strict_off")
      } icon: {
        Image(systemName: strict.isStrict ? "lock.shield" : "lock")
      }
      hint("channel_strict_hint")
      if strict.isStrict {
        ForEach(strict.roles, id: \.userId) { person in
          row(person.maybeName, detail: person.isAdmin ? "channel_strict_role_admin" : "channel_strict_role_moderator")
        }
        if strict.canTurnOff {
          Button("channel_strict_turn_off", role: .destructive) { maybeConfirming = .turnOff }
            .disabled(isRunning)
        }
      } else if !strict.canTurnOn {
        hint("channel_strict_ask_workspace_admin")
      } else {
        lossesContent
        stepsContent
      }
      if hasFailed { failureNote }
    } header: {
      Text("channel_strict_title")
    }
    .confirmationDialog(
      Text(confirmTitle),
      isPresented: Binding(get: { maybeConfirming != nil }, set: { if !$0 { maybeConfirming = nil } }),
      titleVisibility: .visible
    ) {
      if let step = maybeConfirming {
        Button(LocalizedStringKey(confirmButton(step)), role: isDestructive(step) ? .destructive : nil) { run(step) }
        Button("cancel", role: .cancel) { maybeConfirming = nil }
      }
    }
  }

  @ViewBuilder
  private var lossesContent: some View {
    if strict.losses.isEmpty {
      hint("channel_strict_nobody_loses")
    } else {
      hint("channel_strict_losses")
      ForEach(strict.losses, id: \.userId) { loss in
        row(
          loss.maybeName,
          detail: loss.loss == .changingMembers ? "channel_strict_loses_changing" : "channel_strict_loses_adding")
      }
    }
  }

  @ViewBuilder
  private var stepsContent: some View {
    if !strict.hasAdmin {
      hint("channel_strict_needs_admin")
    } else if let readiness = strict.maybeReadiness {
      if readiness.isReady {
        Button("channel_strict_turn_on") { maybeConfirming = .turnOn }
          .buttonStyle(.borderedProminent)
          .disabled(isRunning)
      } else if !readiness.hasAsked {
        hint("channel_strict_ask_hint")
        Button("channel_strict_ask") { run(.askReady) }
          .buttonStyle(.borderedProminent)
          .disabled(isRunning)
      } else {
        Text(String(format: String(localized: "channel_strict_waiting %lld"), readiness.waiting.count))
          .accessibilityAddTraits(.updatesFrequently)
        if readiness.refreshingCount > 0 {
          Text(String(format: String(localized: "channel_strict_refreshing %lld"), Int(readiness.refreshingCount)))
            .font(.app(.footnote)).foregroundStyle(Theme.textMuted)
        }
        ForEach(readiness.waiting, id: \.deviceId) { device in
          HStack {
            row(device.maybeName, detail: "channel_strict_blocker_device")
            if strict.canRemoveBlocker {
              Button("channel_strict_remove_device", role: .destructive) {
                maybeConfirming = .removeBlocker(userId: device.userId, deviceId: device.deviceId)
              }
              .buttonStyle(.borderless)
              .disabled(isRunning)
            }
          }
        }
      }
    }
  }

  private var failureNote: some View {
    Text("error_generic")
      .font(.app(.footnote))
      .foregroundStyle(Theme.danger)
      .accessibilityAddTraits(.isStaticText)
  }

  private var confirmTitle: String {
    switch maybeConfirming {
    case .turnOn: return String(localized: "channel_strict_turn_on_confirm")
    case .turnOff: return String(localized: "channel_strict_turn_off_confirm")
    case .removeBlocker(let userId, _):
      let name =
        strict.maybeReadiness?.waiting.first { $0.userId == userId }?.maybeName
        ?? String(localized: "channel_strict_someone")
      return String(format: String(localized: "channel_strict_remove_device_confirm %@"), name)
    default: return ""
    }
  }

  private func confirmButton(_ step: StrictStepItem) -> String {
    switch step {
    case .turnOn: return "channel_strict_turn_on"
    case .turnOff: return "channel_strict_turn_off"
    case .removeBlocker: return "channel_strict_remove_device"
    default: return "channel_strict_continue"
    }
  }

  private func isDestructive(_ step: StrictStepItem) -> Bool {
    if case .turnOn = step { return false }
    return true
  }

  private func run(_ step: StrictStepItem) {
    isRunning = true
    hasFailed = false
    Task {
      let isDone = await model.strictStep(conversationId, step)
      isRunning = false
      hasFailed = !isDone
      maybeConfirming = nil
    }
  }

  private func row(_ maybeName: String?, detail: LocalizedStringKey) -> some View {
    HStack {
      if let name = maybeName {
        Text(verbatim: name).foregroundStyle(Theme.text).lineLimit(1)
      } else {
        Text("channel_strict_someone").foregroundStyle(Theme.textMuted).lineLimit(1)
      }
      Spacer()
      Text(detail).font(.app(.footnote)).foregroundStyle(Theme.textMuted)
    }
    .accessibilityElement(children: .combine)
  }

  private func hint(_ key: LocalizedStringKey) -> some View {
    Text(key).font(.app(.footnote)).foregroundStyle(Theme.textMuted)
  }
}

/// The notices at the top of a channel: this device stopped at a refused change (D4), or a welcome
/// back waits for the user's word.
struct StrictChannelNotice: View {
  @Environment(AppModel.self) private var model
  let conversationId: String

  @State private var maybeStrict: StrictChannelItem?
  @State private var isRunning = false
  @State private var hasFailed = false
  @State private var isConfirmingRemoval = false

  var body: some View {
    VStack(spacing: 0) {
      if let strict = maybeStrict {
        if let stop = strict.maybeStop { stopNotice(strict, stop) }
        if let rejoin = strict.maybeRejoin { rejoinNotice(rejoin) }
      }
    }
    .task(id: "\(conversationId)/\(model.changeCount)") {
      maybeStrict = await model.strictChannel(conversationId)
    }
  }

  private func stopNotice(_ strict: StrictChannelItem, _ stop: StrictStopItem) -> some View {
    notice(systemImage: "exclamationmark.shield") {
      Text("channel_strict_stopped")
      if strict.canContinue {
        HStack {
          if let signerId = stop.maybeSignerId {
            let name = stop.maybeSignerName ?? String(localized: "channel_strict_someone")
            Button(String(format: String(localized: "channel_strict_remove_and_continue %@"), name)) {
              isConfirmingRemoval = true
            }
            .disabled(isRunning)
            .confirmationDialog(
              Text(String(format: String(localized: "channel_strict_remove_and_continue_confirm %@"), name)),
              isPresented: $isConfirmingRemoval,
              titleVisibility: .visible
            ) {
              Button(String(format: String(localized: "channel_strict_remove_and_continue %@"), name), role: .destructive) {
                run(.continue(maybeRemoveUserId: signerId))
              }
              Button("cancel", role: .cancel) {}
            }
          }
          Button("channel_strict_continue") { run(.continue(maybeRemoveUserId: nil)) }
            .disabled(isRunning)
        }
        .buttonStyle(.bordered)
      }
      if hasFailed { Text("error_generic").foregroundStyle(Theme.danger) }
    }
  }

  private func rejoinNotice(_ rejoin: StrictRejoinItem) -> some View {
    notice(systemImage: "lock.shield") {
      if rejoin.isAccepted {
        Text("channel_strict_rejoin_accepted")
      } else if rejoin.isDue {
        let sender = rejoin.maybeSenderName ?? String(localized: "channel_strict_someone")
        Text(String(format: String(localized: "channel_strict_rejoin_due %@"), sender))
        if !rejoin.joinedNames.isEmpty {
          Text(String(format: String(localized: "channel_strict_rejoin_joined %@"), rejoin.joinedNames.joined(separator: ", ")))
        }
        if !rejoin.goneNames.isEmpty {
          Text(String(format: String(localized: "channel_strict_rejoin_gone %@"), rejoin.goneNames.joined(separator: ", ")))
        }
        Button("channel_strict_rejoin") { run(.acceptRejoin) }
          .buttonStyle(.bordered)
          .disabled(isRunning)
        if hasFailed { Text("error_generic").foregroundStyle(Theme.danger) }
      } else {
        Text("channel_strict_rejoin_waiting")
      }
    }
  }

  private func notice<Content: View>(systemImage: String, @ViewBuilder content: () -> Content) -> some View {
    HStack(alignment: .top, spacing: 8) {
      Image(systemName: systemImage).accessibilityHidden(true)
      VStack(alignment: .leading, spacing: 6) { content() }
    }
    .font(.app(.footnote))
    .foregroundStyle(Theme.text)
    .frame(maxWidth: .infinity, alignment: .leading)
    .padding(.horizontal)
    .padding(.vertical, 8)
    .background(Theme.surface)
  }

  private func run(_ step: StrictStepItem) {
    isRunning = true
    hasFailed = false
    Task {
      hasFailed = !(await model.strictStep(conversationId, step))
      isRunning = false
    }
  }
}

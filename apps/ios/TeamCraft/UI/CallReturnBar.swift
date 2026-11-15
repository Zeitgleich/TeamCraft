import SwiftUI

/// The bar at the top of the app while the call screen is put away (#2066): who the call is with,
/// how long it runs (or that it reconnects), mute and hang up. A tap anywhere else on it brings the
/// call screen back. The call itself never stops for this; nothing is joined again.
struct CallReturnBar: View {
  @Environment(AppModel.self) private var model
  let call: ActiveCall

  /// The organizer is asked whom to hand the meeting to before leaving, as on the call screen.
  @State private var maybeHandover: HandoverOffer?
  @State private var isCheckingLeave = false

  /// The in-call green of the system's own "back to the call" pill, dark enough for white text.
  private static let barColor = Color(red: 0.12, green: 0.48, blue: 0.31)

  var body: some View {
    HStack(spacing: 12) {
      Button { model.calls.restore() } label: {
        HStack(spacing: 10) {
          Image(systemName: call.isOnHold ? "pause.fill" : "phone.fill")
            .font(.system(size: 15, weight: .semibold))
            .accessibilityHidden(true)
          VStack(alignment: .leading, spacing: 1) {
            Text(call.displayName)
              .font(.app(.subheadline).weight(.semibold))
              .lineLimit(1)
            status
              .font(.app(.caption))
              .opacity(0.85)
          }
          Spacer(minLength: 0)
        }
        .frame(maxWidth: .infinity, minHeight: 44, alignment: .leading)
        .contentShape(Rectangle())
      }
      .buttonStyle(.plain)
      .accessibilityElement(children: .combine)
      .accessibilityLabel(Text("call_return \(call.displayName)"))
      .accessibilityHint(Text("call_return_hint"))
      roundButton(
        system: call.isMuted ? "mic.slash.fill" : "mic.fill",
        label: call.isMuted ? "call_unmute" : "call_mute",
        background: .white.opacity(call.isMuted ? 0.9 : 0.2),
        foreground: call.isMuted ? Self.barColor : .white
      ) {
        model.calls.setMuted(!call.isMuted)
      }
      .disabled(call.phase != .connected && call.phase != .reconnecting)
      roundButton(system: "phone.down.fill", label: "call_hang_up", background: Theme.danger, foreground: .white) {
        leave()
      }
      .disabled(isCheckingLeave)
    }
    .foregroundStyle(.white)
    .padding(.horizontal, 16)
    .padding(.vertical, 6)
    .frame(maxWidth: .infinity)
    .background(Self.barColor.ignoresSafeArea(edges: .top))
    .confirmationDialog(
      "call_hosts_leave_title", isPresented: handoverBinding, titleVisibility: .visible, presenting: maybeHandover
    ) { offer in
      ForEach(offer.candidates) { person in
        Button("call_hosts_leave_hand_over \(person.displayName)") { handOverAndLeave(to: person) }
      }
      Button("call_hosts_leave_anyway", role: .destructive) { model.calls.hangUp() }
      Button("cancel", role: .cancel) {}
    } message: { offer in
      Text("call_hosts_leave_body \(offer.waiting)")
    }
  }

  @ViewBuilder
  private var status: some View {
    if call.isOnHold {
      Text("call_on_hold")
    } else {
      switch call.phase {
      case .ringing: Text("call_ringing")
      case .connecting: Text("call_connecting")
      case .reconnecting: Text("call_reconnecting")
      case .connected:
        if let start = call.maybeConnectedAt {
          TimelineView(.periodic(from: start, by: 1)) { context in
            Text(CallScreen.format(context.date.timeIntervalSince(start))).monospacedDigit()
          }
        } else {
          Text("call_connecting")
        }
      }
    }
  }

  private func roundButton(
    system: String, label: LocalizedStringKey, background: Color, foreground: Color, action: @escaping () -> Void
  ) -> some View {
    Button(action: action) {
      Image(systemName: system)
        .font(.system(size: 15, weight: .semibold))
        .foregroundStyle(foreground)
        .frame(width: 36, height: 36)
        .background(background, in: Circle())
        // The touch target stays 44 pt although the circle is smaller.
        .frame(width: 44, height: 44)
        .contentShape(Rectangle())
    }
    .buttonStyle(.plain)
    .accessibilityLabel(Text(label))
  }

  /// Hang up, with the organizer's hand-over question first, as on the call screen.
  private func leave() {
    UIImpactFeedbackGenerator(style: .light).impactOccurred()
    guard call.phase == .connected else {
      model.calls.hangUp()
      return
    }
    isCheckingLeave = true
    let conversationId = call.conversationId
    let participants = call.participantUserIds
    Task {
      defer { isCheckingLeave = false }
      guard let hosts = await model.meetingHosts(conversationId: conversationId), hosts.ownPart == .organizer,
        let offer = await model.handoverOffer(conversationId: conversationId, participantIds: participants, hosts: hosts)
      else {
        model.calls.hangUp()
        return
      }
      maybeHandover = offer
    }
  }

  private func handOverAndLeave(to person: CallPerson) {
    let conversationId = call.conversationId
    Task {
      await model.handOverBeforeLeaving(conversationId: conversationId, userId: person.id)
      model.calls.hangUp()
    }
  }

  private var handoverBinding: Binding<Bool> {
    Binding(get: { maybeHandover != nil }, set: { if !$0 { maybeHandover = nil } })
  }
}

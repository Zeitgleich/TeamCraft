import SwiftUI

/// The in-call screen: who, state, the privacy code, mute and hang up. The system's call UI
/// (lock screen, CarPlay) drives the same CallKit actions.
struct CallScreen: View {
  @Environment(AppModel.self) private var model
  let call: ActiveCall

  var body: some View {
    VStack(spacing: 24) {
      Spacer()
      AvatarView(name: call.displayName, size: 96)
      Text(call.displayName).font(.title.bold())
      Text(statusText).foregroundStyle(Theme.textMuted)
      if call.isSecured {
        Label("call_secured", systemImage: "lock.fill").foregroundStyle(Theme.accent)
      }
      if let code = call.maybePrivacyCode {
        VStack(spacing: 6) {
          Text("call_privacy_code_hint").font(.footnote).foregroundStyle(Theme.textMuted).multilineTextAlignment(.center)
          Text(code).font(.system(size: 24, weight: .bold, design: .monospaced))
        }
      }
      if call.participantCount > 0 {
        Text("call_participants \(Int(call.participantCount))").foregroundStyle(Theme.textMuted)
      }
      Spacer()
      HStack(spacing: 48) {
        roundButton(
          system: call.isMuted ? "mic.slash.fill" : "mic.fill",
          color: call.isMuted ? Theme.accent : Theme.surface,
          label: call.isMuted ? "call_unmute" : "call_mute"
        ) { model.calls.setMuted(!call.isMuted) }
        roundButton(system: "phone.down.fill", color: Theme.danger, label: "call_hang_up") {
          model.calls.hangUp()
        }
      }
      .padding(.bottom, 32)
    }
    .frame(maxWidth: .infinity, maxHeight: .infinity)
    .background(Theme.background.ignoresSafeArea())
  }

  private var statusText: LocalizedStringKey {
    switch call.phase {
    case .ringing: return "call_ringing"
    case .connecting: return call.isSecured ? "call_connecting" : "call_securing"
    case .connected: return "call_ongoing"
    }
  }

  private func roundButton(
    system: String, color: Color, label: LocalizedStringKey, action: @escaping () -> Void
  ) -> some View {
    Button(action: action) {
      Image(systemName: system)
        .font(.title)
        .foregroundStyle(color == Theme.surface ? Theme.text : .white)
        .frame(width: 72, height: 72)
        .background(color)
        .clipShape(Circle())
    }
    .accessibilityLabel(Text(label))
  }
}

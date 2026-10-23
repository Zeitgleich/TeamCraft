import SwiftUI

/// The in-call screen: who, state, the privacy code, mute and hang up. The system's call UI
/// (lock screen, CarPlay) drives the same CallKit actions.
struct CallScreen: View {
  @Environment(AppModel.self) private var model
  let call: ActiveCall

  var body: some View {
    ZStack {
      if let stream = model.calls.remoteVideo.first(where: { !$0.isScreen }) ?? model.calls.remoteVideo.first {
        RemoteVideoView(renderer: stream.renderer).ignoresSafeArea()
      }
      controls
      if model.calls.isCameraOn {
        CameraPreviewView(session: model.calls.camera.captureSession)
          .frame(width: 108, height: 144)
          .clipShape(RoundedRectangle(cornerRadius: Theme.radius))
          .overlay(RoundedRectangle(cornerRadius: Theme.radius).stroke(.white.opacity(0.6)))
          .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topTrailing)
          .padding()
      }
    }
    .frame(maxWidth: .infinity, maxHeight: .infinity)
    .background(Theme.background.ignoresSafeArea())
    .alert("call_camera_denied", isPresented: cameraDeniedBinding) {
      Button("push_denied_open_settings") {
        if let url = URL(string: UIApplication.openSettingsURLString) { UIApplication.shared.open(url) }
      }
      Button("cancel", role: .cancel) {}
    }
  }

  private var hasRemoteVideo: Bool { !model.calls.remoteVideo.isEmpty }

  private var cameraDeniedBinding: Binding<Bool> {
    Binding(get: { model.calls.isCameraDenied }, set: { if !$0 { model.calls.dismissCameraDenied() } })
  }

  private var controls: some View {
    VStack(spacing: 24) {
      Spacer()
      if !hasRemoteVideo { AvatarView(name: call.displayName, size: 96) }
      Text(call.displayName).font(.title.bold())
      Text(statusText).foregroundStyle(Theme.textMuted)
      if call.isSecured {
        Label("call_secured", systemImage: "lock.fill").foregroundStyle(Theme.accent)
      }
      if let code = call.maybePrivacyCode {
        VStack(spacing: 6) {
          Text("call_privacy_code_hint").font(.footnote).foregroundStyle(Theme.textMuted).multilineTextAlignment(.center)
          Text(code).font(.system(.title2, design: .monospaced).bold())
        }
      }
      if call.participantCount > 0 {
        Text("call_participants \(Int(call.participantCount))").foregroundStyle(Theme.textMuted)
      }
      Spacer()
      HStack(spacing: 32) {
        roundButton(
          system: call.isMuted ? "mic.slash.fill" : "mic.fill",
          color: call.isMuted ? Theme.accent : Theme.surface,
          label: call.isMuted ? "call_unmute" : "call_mute"
        ) { model.calls.setMuted(!call.isMuted) }
        roundButton(
          system: model.calls.isCameraOn ? "video.fill" : "video.slash.fill",
          color: model.calls.isCameraOn ? Theme.accent : Theme.surface,
          label: model.calls.isCameraOn ? "call_camera_off" : "call_camera_on"
        ) { Task { await model.calls.setCameraEnabled(!model.calls.isCameraOn) } }
        if model.calls.isCameraOn {
          roundButton(system: "camera.rotate.fill", color: Theme.surface, label: "call_camera_flip") {
            model.calls.flipCamera()
          }
        }
        roundButton(system: "phone.down.fill", color: Theme.danger, label: "call_hang_up") {
          model.calls.hangUp()
        }
      }
      .padding(.bottom, 32)
    }
    .frame(maxWidth: .infinity, maxHeight: .infinity)
  }

  private var statusText: LocalizedStringKey {
    if call.isOnHold { return "call_on_hold" }
    switch call.phase {
    case .ringing: return "call_ringing"
    case .connecting: return call.isSecured ? "call_connecting" : "call_securing"
    case .connected: return "call_ongoing"
    case .reconnecting: return "call_reconnecting"
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

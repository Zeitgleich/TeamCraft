import SwiftUI

/// The in-call screen: who, state, the privacy code, mute and hang up. The system's call UI
/// (lock screen, CarPlay) drives the same CallKit actions. DM and group DM calls can add people,
/// which moves the call into a meeting room (docs/meetings.md, "A call becomes a meeting").
struct CallScreen: View {
  @Environment(AppModel.self) private var model
  let call: ActiveCall

  /// The call is in a DM or group DM.
  @State private var canAddPeople = false
  @State private var isAddingPeople = false

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
    .alert(
      "call_move_failed", isPresented: moveFailureBinding, presenting: model.calls.maybeMoveFailure
    ) { _ in
      Button("done", role: .cancel) {}
    } message: { failure in
      Text(failure.message)
    }
    .sheet(isPresented: $isAddingPeople) { AddPeopleView(conversationId: call.conversationId) }
    .task(id: call.conversationId) {
      let kind = await model.conversations().first { $0.id == call.conversationId }?.kind
      canAddPeople = kind == .direct || kind == .group
    }
  }

  private var moveFailureBinding: Binding<Bool> {
    Binding(get: { model.calls.maybeMoveFailure != nil }, set: { if !$0 { model.calls.dismissMoveFailure() } })
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
      if canAddPeople && call.phase == .connected && model.calls.maybeMoveOffer == nil && !model.calls.isMoving {
        Button("call_add_people", systemImage: "person.badge.plus") { isAddingPeople = true }
          .buttonStyle(.bordered)
      }
      Spacer()
      if let offer = model.calls.maybeMoveOffer { moveBanner(offer) }
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

  /// Somebody moved the call into a meeting room: who, whom they added, and the way over.
  private func moveBanner(_ offer: CallMoveOffer) -> some View {
    VStack(spacing: 12) {
      if offer.addedNames.isEmpty {
        Text("call_moved_by \(offer.byName)")
      } else {
        Text("call_moved_by \(offer.byName) added \(ListFormatter.localizedString(byJoining: offer.addedNames))")
      }
      Button {
        model.calls.joinMovedCall()
      } label: {
        if model.calls.isMoving { ProgressView() } else { Text("call_move_join") }
      }
      .buttonStyle(.borderedProminent)
      .disabled(model.calls.isMoving)
    }
    .multilineTextAlignment(.center)
    .padding()
    .frame(maxWidth: .infinity)
    .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radius))
    .padding(.horizontal)
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

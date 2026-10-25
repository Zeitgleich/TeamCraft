import AVKit
import SwiftUI

/// The in-call screen: an edge-to-edge dark stage with the others' video (one full-bleed, several
/// as a spotlight with a filmstrip) or a large avatar, the name, time and encryption chip on top,
/// round controls at the bottom and the self-view as a corner picture that can be thrown to
/// another corner. A tap on the stage hides or shows the chrome. The system's call UI (lock
/// screen, CarPlay) drives the same CallKit actions. DM and group DM calls can add people, which
/// moves the call into a meeting room (docs/meetings.md, "A call becomes a meeting").
struct CallScreen: View {
  @Environment(AppModel.self) private var model
  @Environment(\.accessibilityReduceMotion) private var reduceMotion
  let call: ActiveCall

  /// The call is in a DM or group DM.
  @State private var canAddPeople = false
  @State private var isAddingPeople = false
  @State private var areControlsShown = true
  @State private var isEncryptionShown = false
  @State private var maybeConnectedAt: Date?
  @State private var maybeSpotlightId: String?
  /// Counts taps on controls, so the chrome stays while the user is busy with it.
  @State private var interactions = 0

  private static let controlSize: CGFloat = 60
  private static let autoHideDelay: Duration = .seconds(5)
  private let stageColor = Color(red: 0.043, green: 0.051, blue: 0.063)

  var body: some View {
    ZStack {
      stageColor.ignoresSafeArea()
      stage
        .ignoresSafeArea()
        .contentShape(Rectangle())
        .onTapGesture { setControlsShown(!areControlsShown) }
        .accessibilityAddTraits(.isButton)
        .accessibilityLabel(Text("call_toggle_controls"))
      chrome
      if model.calls.isCameraOn {
        SelfViewCorner(size: CGSize(width: 108, height: 144), spring: spring) {
          CameraPreviewView(session: model.calls.camera.captureSession)
            .clipShape(RoundedRectangle(cornerRadius: 20, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: 20, style: .continuous).stroke(.white.opacity(0.35)))
        }
      }
    }
    .frame(maxWidth: .infinity, maxHeight: .infinity)
    .preferredColorScheme(.dark)
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
    .sheet(isPresented: $isEncryptionShown) { encryptionSheet }
    .task(id: call.conversationId) {
      let kind = await model.conversations().first { $0.id == call.conversationId }?.kind
      canAddPeople = kind == .direct || kind == .group
    }
    .onChange(of: call.phase, initial: true) {
      // The clock starts with the first connection and runs through reconnects.
      if call.phase == .connected && maybeConnectedAt == nil { maybeConnectedAt = Date() }
    }
    .task(id: AutoHideKey(isShown: areControlsShown, hasVideo: hasRemoteVideo, phase: call.phase, interactions: interactions)) {
      // The chrome steps aside when there is video to look at; without video it stays.
      guard !hasRemoteVideo else {
        if areControlsShown && call.phase == .connected {
          try? await Task.sleep(for: Self.autoHideDelay)
          if !Task.isCancelled { setControlsShown(false) }
        }
        return
      }
      if !areControlsShown { setControlsShown(true) }
    }
  }

  // MARK: Stage

  @ViewBuilder
  private var stage: some View {
    if let spotlight {
      ZStack(alignment: .bottom) {
        RemoteVideoView(renderer: spotlight.renderer)
          .overlay(alignment: .bottomLeading) { nameTag(for: spotlight) }
        if filmstrip.count > 0 { filmstripView }
      }
    } else {
      avatarStage
    }
  }

  /// Without video: the people in the call as avatars with their names; whoever speaks has a ring.
  private var avatarStage: some View {
    let people = call.participantUserIds.map { id in
      StagePerson(
        id: id, name: model.calls.names[id] ?? "",
        isSpeaking: call.speakingUserIds.contains(id),
        isRejoining: call.reconnectingUserIds.contains(id))
    }
    // Alone in the call (or still connecting): the one being called.
    let shown =
      people.isEmpty
      ? [StagePerson(id: "", name: call.displayName, isSpeaking: false, isRejoining: false)] : people
    let size: CGFloat = shown.count <= 1 ? 128 : (shown.count <= 4 ? 96 : 72)
    return ScrollView {
      LazyVGrid(
        columns: [GridItem(.adaptive(minimum: size + 24), spacing: 20)], spacing: 20
      ) {
        ForEach(shown) { person in
          VStack(spacing: 8) {
            AvatarView(name: person.name, size: size)
              .padding(4)
              .overlay(
                Circle().stroke(Theme.accent, lineWidth: person.isSpeaking ? 3 : 0)
              )
              .animation(.easeOut(duration: 0.15), value: person.isSpeaking)
            Text(person.name).font(.footnote.weight(.medium)).foregroundStyle(.white).lineLimit(1)
          }
          .opacity(person.isRejoining ? 0.5 : 1)
          .accessibilityElement(children: .combine)
        }
      }
      .padding(32)
      .frame(maxWidth: .infinity, minHeight: UIScreen.main.bounds.height * 0.6)
    }
    .scrollBounceBehavior(.basedOnSize)
  }

  /// The name of the person whose device sends `stream`, once known.
  private func ownerName(of stream: RemoteVideoStream) -> String? {
    call.userIdByDevice[stream.deviceId].flatMap { model.calls.names[$0] }
  }

  private func isSpeaking(_ stream: RemoteVideoStream) -> Bool {
    call.userIdByDevice[stream.deviceId].map { call.speakingUserIds.contains($0) } ?? false
  }

  private var filmstripView: some View {
    ScrollView(.horizontal, showsIndicators: false) {
      HStack(spacing: 8) {
        ForEach(filmstrip) { stream in
          RemoteVideoView(renderer: stream.renderer)
            .frame(width: 104, height: 78)
            .overlay(alignment: .bottomLeading) { nameTag(for: stream) }
            .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
            .overlay(
              RoundedRectangle(cornerRadius: 14, style: .continuous)
                .stroke(Theme.accent, lineWidth: isSpeaking(stream) ? 3 : 0))
            .onTapGesture { withAnimation(spring) { maybeSpotlightId = stream.id } }
            .accessibilityAddTraits(.isButton)
            .accessibilityLabel(Text("call_video_spotlight"))
        }
      }
      .padding(.horizontal, 16)
    }
    .padding(.bottom, 128)
    .frame(height: 78 + 128, alignment: .top)
  }

  @ViewBuilder
  private func nameTag(for stream: RemoteVideoStream) -> some View {
    if let name = ownerName(of: stream) {
      Text(name)
        .font(.caption.weight(.medium))
        .foregroundStyle(.white)
        .lineLimit(1)
        .padding(.horizontal, 8)
        .padding(.vertical, 2)
        .background(.black.opacity(0.55), in: Capsule())
        .padding(8)
    }
  }

  /// Shared screens first: they are what people look at.
  private var orderedStreams: [RemoteVideoStream] {
    model.calls.remoteVideo.sorted { $0.isScreen && !$1.isScreen }
  }

  private var spotlight: RemoteVideoStream? {
    orderedStreams.first { $0.id == maybeSpotlightId } ?? orderedStreams.first
  }

  private var filmstrip: [RemoteVideoStream] {
    orderedStreams.filter { $0.id != spotlight?.id }
  }

  private var hasRemoteVideo: Bool { !model.calls.remoteVideo.isEmpty }

  // MARK: Chrome

  private var chrome: some View {
    VStack(spacing: 0) {
      if areControlsShown { topBar.transition(chromeTransition(edge: .top)) }
      Spacer(minLength: 0)
      VStack(spacing: 12) {
        statusHint
        if let offer = model.calls.maybeMoveOffer { moveBanner(offer) }
      }
      .padding(.horizontal, 16)
      if areControlsShown { controlBar.transition(chromeTransition(edge: .bottom)) }
    }
    .frame(maxWidth: .infinity, maxHeight: .infinity)
  }

  private var topBar: some View {
    VStack(spacing: 6) {
      Text(call.displayName)
        .font(.title2.weight(.semibold))
        .lineLimit(1)
        .foregroundStyle(.white)
      subtitle
        .font(.subheadline)
        .foregroundStyle(.white.opacity(0.7))
        .accessibilityAddTraits(.updatesFrequently)
      if call.isSecured || call.maybePrivacyCode != nil {
        Button { isEncryptionShown = true } label: {
          Label(LocalizedStringKey(call.isSecured ? "call_secured" : "call_securing"), systemImage: "lock.fill")
            .font(.footnote.weight(.semibold))
            .foregroundStyle(.white)
            .padding(.horizontal, 12)
            .frame(minHeight: 44)
            .background(.ultraThinMaterial, in: Capsule())
        }
        .accessibilityHint(Text("call_encryption_open"))
      }
    }
    .padding(.horizontal, 16)
    .padding(.top, 8)
    .frame(maxWidth: .infinity)
    .background(alignment: .top) {
      LinearGradient(colors: [.black.opacity(0.5), .clear], startPoint: .top, endPoint: .bottom)
        .ignoresSafeArea()
    }
  }

  @ViewBuilder
  private var subtitle: some View {
    if call.isOnHold {
      Text("call_on_hold")
    } else {
      switch call.phase {
      case .ringing: Text("call_ringing")
      case .connecting: Text(LocalizedStringKey(call.isSecured ? "call_connecting" : "call_securing"))
      case .reconnecting: Text("call_reconnecting")
      case .connected:
        HStack(spacing: 4) {
          if let start = maybeConnectedAt {
            TimelineView(.periodic(from: start, by: 1)) { context in
              Text(Self.format(context.date.timeIntervalSince(start)))
                .monospacedDigit()
            }
          }
          if call.participantCount > 0 {
            Text("·")
            Text("call_participants \(Int(call.participantCount))")
          }
        }
      }
    }
  }

  /// What matters while talking: that the connection is gone, and that nobody hears you.
  @ViewBuilder
  private var statusHint: some View {
    if call.phase == .reconnecting {
      hint("call_reconnecting")
    } else if call.isMuted && call.phase == .connected {
      hint("call_you_muted")
    }
  }

  private func hint(_ key: LocalizedStringKey) -> some View {
    Text(key)
      .font(.subheadline.weight(.medium))
      .foregroundStyle(.white)
      .padding(.horizontal, 16)
      .padding(.vertical, 8)
      .background(.black.opacity(0.55), in: Capsule())
      .accessibilityAddTraits(.updatesFrequently)
  }

  private var controlBar: some View {
    HStack(spacing: 0) {
      Spacer(minLength: 0)
      RoundControl(
        system: call.isMuted ? "mic.slash.fill" : "mic.fill",
        isActive: call.isMuted,
        label: call.isMuted ? "call_unmute" : "call_mute"
      ) { interact { model.calls.setMuted(!call.isMuted) } }
      Spacer(minLength: 0)
      RoundControl(
        system: model.calls.isCameraOn ? "video.fill" : "video.slash.fill",
        isActive: !model.calls.isCameraOn,
        label: model.calls.isCameraOn ? "call_camera_off" : "call_camera_on"
      ) { interact { Task { await model.calls.setCameraEnabled(!model.calls.isCameraOn) } } }
      if model.calls.isCameraOn {
        Spacer(minLength: 0)
        RoundControl(system: "camera.rotate.fill", isActive: false, label: "call_camera_flip") {
          interact { model.calls.flipCamera() }
        }
      }
      Spacer(minLength: 0)
      AudioRouteButton().frame(width: Self.controlSize, height: Self.controlSize)
      if canAddPeople && call.phase == .connected && model.calls.maybeMoveOffer == nil && !model.calls.isMoving {
        Spacer(minLength: 0)
        RoundControl(system: "person.badge.plus", isActive: false, label: "call_add_people") {
          interact { isAddingPeople = true }
        }
      }
      Spacer(minLength: 0)
      RoundControl(system: "phone.down.fill", isActive: false, label: "call_hang_up", tint: Theme.danger) {
        model.calls.hangUp()
      }
      Spacer(minLength: 0)
    }
    .padding(.vertical, 16)
    .background(alignment: .bottom) {
      LinearGradient(colors: [.clear, .black.opacity(0.55)], startPoint: .top, endPoint: .bottom)
        .ignoresSafeArea()
    }
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
    .foregroundStyle(.white)
    .multilineTextAlignment(.center)
    .padding()
    .frame(maxWidth: .infinity)
    .background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: 20, style: .continuous))
  }

  /// The end-to-end encryption explained, with the code to compare out loud.
  private var encryptionSheet: some View {
    VStack(spacing: 14) {
      Image(systemName: "lock.fill").font(.largeTitle).foregroundStyle(Theme.accent)
      Text("call_encryption_title").font(.title2.bold())
      Text("call_encryption_body").multilineTextAlignment(.center)
      if let code = call.maybePrivacyCode {
        Text("call_privacy_code_hint")
          .font(.footnote)
          .foregroundStyle(Theme.textMuted)
          .multilineTextAlignment(.center)
        Text(code)
          .font(.system(.title, design: .monospaced).bold())
          .accessibilityLabel(Text(code.map(String.init).joined(separator: " ")))
      }
    }
    .padding(24)
    .presentationDetents([.medium])
    .presentationDragIndicator(.visible)
  }

  // MARK: Helpers

  private var spring: Animation {
    // Critically damped, as the chrome has no momentum of its own; reduced motion cross-fades.
    reduceMotion ? .easeInOut(duration: 0.2) : .spring(response: 0.4, dampingFraction: 1)
  }

  private func chromeTransition(edge: Edge) -> AnyTransition {
    reduceMotion ? .opacity : .move(edge: edge).combined(with: .opacity)
  }

  private func setControlsShown(_ isShown: Bool) {
    withAnimation(spring) { areControlsShown = isShown }
  }

  private func interact(_ action: () -> Void) {
    interactions += 1
    action()
  }

  private var moveFailureBinding: Binding<Bool> {
    Binding(get: { model.calls.maybeMoveFailure != nil }, set: { if !$0 { model.calls.dismissMoveFailure() } })
  }

  private var cameraDeniedBinding: Binding<Bool> {
    Binding(get: { model.calls.isCameraDenied }, set: { if !$0 { model.calls.dismissCameraDenied() } })
  }

  private static func format(_ interval: TimeInterval) -> String {
    let seconds = max(0, Int(interval))
    let (hours, minutes, rest) = (seconds / 3600, seconds % 3600 / 60, seconds % 60)
    return hours > 0
      ? String(format: "%d:%02d:%02d", hours, minutes, rest)
      : String(format: "%d:%02d", minutes, rest)
  }
}

private struct StagePerson: Identifiable {
  let id: String
  let name: String
  let isSpeaking: Bool
  let isRejoining: Bool
}

private struct AutoHideKey: Hashable {
  let isShown: Bool
  let hasVideo: Bool
  let phase: ActiveCall.Phase
  let interactions: Int
}

/// A round icon button (60 pt, above the 44 pt minimum). A switched-on state is a filled white
/// button; it dips on press, at once (feedback on touch-down, not on release).
private struct RoundControl: View {
  let system: String
  let isActive: Bool
  let label: LocalizedStringKey
  var tint: Color?
  let action: () -> Void

  var body: some View {
    Button(action: action) {
      Image(systemName: system)
        .font(.title2)
        .foregroundStyle(tint != nil ? .white : (isActive ? Color.black : .white))
        .frame(width: 60, height: 60)
        .background(tint ?? (isActive ? Color.white : Color.white.opacity(0.16)), in: Circle())
        .contentShape(Circle())
    }
    .buttonStyle(PressScaleStyle())
    .accessibilityLabel(Text(label))
  }
}

private struct PressScaleStyle: ButtonStyle {
  func makeBody(configuration: Configuration) -> some View {
    configuration.label
      .scaleEffect(configuration.isPressed ? 0.94 : 1)
      .animation(.easeOut(duration: 0.1), value: configuration.isPressed)
  }
}

/// The system's audio route picker (speaker, earpiece, Bluetooth, AirPlay) in the controls' style.
private struct AudioRouteButton: View {
  var body: some View {
    ZStack {
      Circle().fill(.white.opacity(0.16))
      RoutePicker().frame(width: 28, height: 28)
    }
    .accessibilityElement(children: .ignore)
    .accessibilityLabel(Text("call_audio_route"))
    .accessibilityAddTraits(.isButton)
  }

  private struct RoutePicker: UIViewRepresentable {
    func makeUIView(context: Context) -> AVRoutePickerView {
      let picker = AVRoutePickerView()
      picker.tintColor = .white
      picker.activeTintColor = .white
      picker.prioritizesVideoDevices = false
      return picker
    }

    func updateUIView(_ view: AVRoutePickerView, context: Context) {}
  }
}

/// The self-view in a corner. Drag it and it lands in the corner the throw is heading for
/// (the release velocity projects the resting point), on a critically damped spring.
private struct SelfViewCorner<Content: View>: View {
  let size: CGSize
  let spring: Animation
  @ViewBuilder let content: Content

  @State private var corner = Corner.topTrailing
  @State private var dragOffset = CGSize.zero

  private enum Corner: CaseIterable { case topLeading, topTrailing, bottomLeading, bottomTrailing }

  private let margin: CGFloat = 12
  /// Room for the top bar and the controls, which the picture should not cover.
  private let topClearance: CGFloat = 130
  private let bottomClearance: CGFloat = 120

  var body: some View {
    GeometryReader { proxy in
      let origin = origin(of: corner, in: proxy)
      content
        .frame(width: size.width, height: size.height)
        .offset(x: origin.x + dragOffset.width, y: origin.y + dragOffset.height)
        .gesture(
          DragGesture()
            .onChanged { dragOffset = $0.translation }
            .onEnded { value in
              let projected = CGPoint(
                x: origin.x + size.width / 2 + value.predictedEndTranslation.width,
                y: origin.y + size.height / 2 + value.predictedEndTranslation.height)
              let target = Corner.allCases.min {
                distance(from: centre(of: $0, in: proxy), to: projected)
                  < distance(from: centre(of: $1, in: proxy), to: projected)
              } ?? corner
              withAnimation(spring) {
                corner = target
                dragOffset = .zero
              }
            }
        )
        .accessibilityHidden(true)
    }
  }

  private func origin(of corner: Corner, in proxy: GeometryProxy) -> CGPoint {
    let area = proxy.size
    let left = proxy.safeAreaInsets.leading + margin
    let right = area.width - proxy.safeAreaInsets.trailing - size.width - margin
    let top = proxy.safeAreaInsets.top + topClearance - 60
    let bottom = area.height - proxy.safeAreaInsets.bottom - bottomClearance - size.height
    switch corner {
    case .topLeading: return CGPoint(x: left, y: top)
    case .topTrailing: return CGPoint(x: right, y: top)
    case .bottomLeading: return CGPoint(x: left, y: bottom)
    case .bottomTrailing: return CGPoint(x: right, y: bottom)
    }
  }

  private func centre(of corner: Corner, in proxy: GeometryProxy) -> CGPoint {
    let origin = origin(of: corner, in: proxy)
    return CGPoint(x: origin.x + size.width / 2, y: origin.y + size.height / 2)
  }

  private func distance(from a: CGPoint, to b: CGPoint) -> CGFloat {
    hypot(a.x - b.x, a.y - b.y)
  }
}

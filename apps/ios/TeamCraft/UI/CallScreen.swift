import AVKit
import SwiftUI

/// The in-call screen: an edge-to-edge dark stage with everybody as a tile of a grid (camera or
/// avatar; a shared screen as a spotlight with a filmstrip), the name, time and encryption chip on top,
/// round controls at the bottom and the self-view as a corner picture that can be thrown to
/// another corner. A tap on the stage hides or shows the chrome. The system's call UI (lock
/// screen, CarPlay) drives the same CallKit actions. DM and group DM calls can add people, which
/// moves the call into a meeting room (docs/meetings.md, "A call becomes a meeting"), and every
/// call can get the notetaker for whoever the policy lets ask it in (e2ee.md §8.2, "In every call").
/// In a meeting's call the tiles say who hosts it, and its organizer names co-hosts, hands it over
/// and is asked to hand it over before leaving people stuck in the lobby (#1720).
struct CallScreen: View {
  @Environment(AppModel.self) private var model
  @Environment(\.accessibilityReduceMotion) private var reduceMotion
  @Environment(\.accessibilityVoiceOverEnabled) private var isVoiceOverOn
  @Environment(\.accessibilitySwitchControlEnabled) private var isSwitchControlOn
  let call: ActiveCall

  /// The call is in a DM or group DM.
  @State private var canAddPeople = false
  @State private var isAddingPeople = false
  /// The notetaker of the verified policy in this call; read again when somebody comes or goes.
  @State private var maybeNotetaker: NotetakerCallInfo?
  /// One per call screen: a retry of "Add notetaker" gets the same room.
  @State private var notetakerKey = newRequestKey()
  /// "Add notetaker" was pressed where the policy doesn't let the user: the reason shows.
  @State private var isNotetakerRefusalShown = false
  @State private var areControlsShown = true
  @State private var isEncryptionShown = false
  @State private var maybeSpotlightId: String?
  /// Counts taps on controls, so the chrome stays while the user is busy with it.
  @State private var interactions = 0
  /// Who hosts the meeting of the call (#1720); `nil` outside a meeting. Read again when somebody
  /// comes or goes.
  @State private var maybeHosts: MeetingHosts?
  @State private var isHostsShown = false
  /// The lobby of the call's meeting for its hosts (#1859); `nil` for anybody else. Read again when
  /// somebody comes or goes and on a knock.
  @State private var maybeLobby: MeetingLobby?
  @State private var isLobbyShown = false
  /// The organizer is asked whom to hand the meeting to before leaving.
  @State private var maybeHandover: HandoverOffer?
  @State private var isCheckingLeave = false
  /// "End for everyone…" was chosen on the hang-up (#1930): the question is shown.
  @State private var isEndAsked = false
  @State private var isEnding = false
  @State private var maybeEndFailure: LocalizedStringKey?
  /// The floating window when the app goes to the background during a video call (#2067).
  @State private var pictureInPicture = CallPictureInPicture()
  /// What is unread in the call's conversation: the Chat button's badge (#1488).
  @State private var chatBadge = CallChatBadge.none
  /// Reactions rising over the stage (#2324): the others' and the user's own.
  @State private var floating: [FloatingReaction] = []
  @State private var nextFloatingKey = 0
  /// The id of the newest reaction of the others already shown; the first look is the baseline.
  @State private var maybeLastReactionId: UInt64?

  private static let controlSize: CGFloat = 60
  private static let autoHideDelay: Duration = .seconds(5)
  private let stageColor = Color(red: 0.043, green: 0.051, blue: 0.063)

  var body: some View {
    ZStack {
      stageColor.ignoresSafeArea()
      PictureInPictureSource(pictureInPicture: pictureInPicture)
        .ignoresSafeArea()
        .accessibilityHidden(true)
      stage
        .ignoresSafeArea()
        .contentShape(Rectangle())
        .onTapGesture { setControlsShown(!areControlsShown) }
        // An action, not a label and a button trait on the container: those could replace the
        // tiles' own labels for VoiceOver (#2281).
        .accessibilityAction(named: Text("call_toggle_controls")) { setControlsShown(!areControlsShown) }
      // Reactions rise over the stage, clear of the header and the controls (#2324).
      FloatingReactionsLayer(reactions: floating, reduceMotion: reduceMotion)
        .padding(.top, Self.gridTopRoom)
        .padding(.bottom, Self.gridBottomRoom)
      chrome
      if model.calls.isCameraOn {
        SelfViewCorner(size: CGSize(width: 108, height: 144), spring: spring) {
          CameraPreviewView(session: model.calls.camera.captureSession)
            .overlay(alignment: .bottom) {
              // #1891: the preview still shows the camera while the others don't get it; the
              // status hint announces it, this is the note on the tile.
              if call.isVideoHeldForVoice && call.phase == .connected {
                Text("call_self_view_held_for_voice")
                  .font(.caption2.weight(.medium))
                  .foregroundStyle(.white)
                  .multilineTextAlignment(.center)
                  .frame(maxWidth: .infinity)
                  .padding(.horizontal, 6)
                  .padding(.vertical, 4)
                  .background(.black.opacity(0.6))
              }
            }
            .clipShape(RoundedRectangle(cornerRadius: 20, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: 20, style: .continuous).stroke(.white.opacity(0.35)))
        }
      }
    }
    .frame(maxWidth: .infinity, maxHeight: .infinity)
    .preferredColorScheme(.dark)
    .onChange(of: pictureInPictureStream?.id, initial: true) {
      pictureInPicture.show(pictureInPictureStream?.renderer)
    }
    .onDisappear { pictureInPicture.stop() }
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
    .alert(
      "call_notetaker_add", isPresented: notetakerFailureBinding, presenting: model.calls.maybeNotetakerFailure
    ) { _ in
      Button("done", role: .cancel) {}
    } message: { failure in
      Text(failure.message)
    }
    .alert("call_notetaker_add", isPresented: $isNotetakerRefusalShown) {
      Button("done", role: .cancel) {}
    } message: {
      if let key = notetakerRefusalKey { Text(key) }
    }
    .sheet(isPresented: $isAddingPeople) { AddPeopleView(conversationId: call.conversationId) }
    .sheet(isPresented: $isEncryptionShown) { encryptionSheet }
    .sheet(isPresented: $isHostsShown) {
      if let hosts = maybeHosts {
        CallHostsSheet(
          conversationId: call.conversationId, participantIds: call.participantUserIds, hosts: hosts
        ) { await readHosts() }
      }
    }
    .sheet(isPresented: $isLobbyShown) {
      if let lobby = maybeLobby {
        CallLobbySheet(conversationId: call.conversationId, lobby: lobby) { await readLobby() }
      }
    }
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
    .confirmationDialog("call_end_title", isPresented: $isEndAsked, titleVisibility: .visible) {
      Button("call_end_do", role: .destructive) { endForEveryone() }
      Button("cancel", role: .cancel) {}
    } message: {
      Text("call_end_body")
    }
    .alert("call_end_failed", isPresented: endFailureBinding) {
      Button("done", role: .cancel) {}
    } message: {
      if let failure = maybeEndFailure { Text(failure) }
    }
    .task(id: call.conversationId) {
      let kind = await model.conversations().first { $0.id == call.conversationId }?.kind
      canAddPeople = kind == .direct || kind == .group
    }
    .onChange(of: call.conversationId) {
      // Another call (the room a move led to): its own key.
      notetakerKey = newRequestKey()
    }
    .task(id: NotetakerKey(conversationId: call.conversationId, participants: call.participantUserIds)) {
      maybeNotetaker = await model.notetakerInCall(
        conversationId: call.conversationId, participants: call.participantUserIds)
      if maybeNotetaker?.isPresent == true { model.calls.notetakerArrived() }
    }
    .task(id: NotetakerKey(conversationId: call.conversationId, participants: call.participantUserIds)) {
      await readHosts()
    }
    .task(id: LobbyKey(conversationId: call.conversationId, participants: call.participantUserIds, knocks: model.calls.lobbyKnocks)) {
      await readLobby()
    }
    .task(id: ChatBadgeKey(conversationId: call.conversationId, changes: model.changeCount)) {
      await readChatBadge()
    }
    .task(id: AutoHideKey(isShown: areControlsShown, hasVideo: hasRemoteVideo, phase: call.phase, interactions: interactions)) {
      // The chrome steps aside when there is video to look at; without video it stays.
      guard !hasRemoteVideo else {
        if areControlsShown && call.phase == .connected && !keepsControlsShown {
          try? await Task.sleep(for: Self.autoHideDelay)
          if !Task.isCancelled { setControlsShown(false) }
        }
        return
      }
      if !areControlsShown { setControlsShown(true) }
    }
    // What VoiceOver users can't see happen (#217); the banners only change in place.
    .onChange(of: call.recordingUserIds) { old, new in
      if new.contains(where: { !old.contains($0) }) {
        announce(String(localized: "call_recording_by \(recordingNames)"), isUrgent: true)
      }
    }
    .onChange(of: call.participantUserIds) { old, new in
      rosterAnnouncer.note(RosterChange(before: old, now: new), names: model.calls.names, say: { announce($0) })
    }
    .onAppear { maybeLastReactionId = call.reactions.last?.id ?? 0 }
    .onChange(of: call.reactions.last?.id) { showNewReactions() }
    .onChange(of: call.raisedHandUserIds) { old, new in
      let raised = new.filter { !old.contains($0) }
      if !raised.isEmpty { announce(String(localized: "call_announce_hand_raised \(names(of: raised))")) }
    }
    .onChange(of: call.phase) { _, phase in
      if phase == .reconnecting { announce(String(localized: "call_reconnecting")) }
    }
    .onChange(of: shownLobby?.people.count ?? 0) { old, new in
      if new > old { announce(String(localized: "call_lobby_banner \(new)")) }
    }
  }

  /// Joins and leaves, said together after a short window (#2281).
  @State private var rosterAnnouncer = RosterAnnouncer()

  /// WCAG 2.2.1 (#217): with VoiceOver or Switch Control the controls don't step aside on a timer;
  /// they would vanish while the user moves towards them.
  private var keepsControlsShown: Bool { isVoiceOverOn || isSwitchControlOn }

  /// Says what changed. Recording is urgent: it is a matter of consent (e2ee.md §8.3).
  private func announce(_ text: String, isUrgent: Bool = false) {
    var announcement = AttributedString(text)
    if isUrgent { announcement.accessibilitySpeechAnnouncementPriority = .high }
    AccessibilityNotification.Announcement(announcement).post()
  }

  /// Floats the others' reactions above the last one shown, and says them to VoiceOver (#2324).
  private func showNewReactions() {
    let seen = maybeLastReactionId ?? 0
    let fresh = call.reactions.filter { $0.id > seen }
    maybeLastReactionId = max(seen, call.reactions.last?.id ?? 0)
    guard !fresh.isEmpty else { return }
    for reaction in fresh {
      float(reaction.emoji, name: model.calls.names[reaction.userId] ?? "")
    }
    if fresh.count > 3 {
      announce(String(localized: "call_announce_reacted_many \(fresh.count)"))
    } else {
      let worded = fresh
        .map { "\(names(of: [$0.userId])) (\(FloatingReactions.name(of: $0.reaction)))" }
        .joined(separator: ", ")
      announce(String(localized: "call_announce_reacted \(worded)"))
    }
  }

  /// Sends a reaction and floats it for the user at once.
  private func react(_ choice: CallReactionChoice) {
    model.calls.sendReaction(choice.reaction)
    float(choice.emoji, name: "")
  }

  private func float(_ emoji: String, name: String) {
    nextFloatingKey += 1
    let key = nextFloatingKey
    if floating.count >= FloatingReactions.maxFloating { floating.removeFirst() }
    floating.append(FloatingReaction(id: key, emoji: emoji, name: name, xFraction: FloatingReactions.spread(key)))
    Task { @MainActor in
      try? await Task.sleep(for: .seconds(FloatingReactions.seconds))
      floating.removeAll { $0.id == key }
    }
  }

  /// The names of `ids`, "Someone" for those without one, joined.
  private func names(of ids: [String]) -> String {
    let someone = String(localized: "call_recording_someone")
    return ids.map { id in model.calls.names[id].flatMap { $0.isEmpty ? nil : $0 } ?? someone }.joined(separator: ", ")
  }

  // MARK: Stage

  @ViewBuilder
  private var stage: some View {
    // A shared screen gets the stage; otherwise everybody is a tile of the grid (#1488).
    if !orderedStreams.contains(where: \.isScreen) {
      gridStage
    } else if let spotlight {
      ZStack(alignment: .bottom) {
        RemoteVideoView(renderer: spotlight.renderer)
          .overlay { videoNotice(for: spotlight) }
          .overlay(alignment: .bottomLeading) { nameTag(for: spotlight) }
        if filmstrip.count > 0 { filmstripView }
      }
    } else {
      gridStage
    }
  }

  /// The call as a grid (#1488), as on Android: a tile with rounded corners per person, their camera
  /// when it is on, their avatar otherwise, and the name on the tile; whoever speaks has a ring. Up
  /// to six tiles share the screen; more scroll in two columns. The top bar and the controls keep
  /// their room.
  private var gridStage: some View {
    let tiles = stageTiles
    let columns = tiles.count <= 2 ? 1 : 2
    let rows = (tiles.count + columns - 1) / columns
    return GeometryReader { proxy in
      let free = proxy.size.height - Self.gridTopRoom - Self.gridBottomRoom - CGFloat(rows - 1) * 8
      let tileHeight =
        tiles.count <= 6
        ? max(free / CGFloat(rows), 120)
        : (proxy.size.width - 24 - 8) / 2 / 0.8
      ScrollView {
        LazyVGrid(columns: Array(repeating: GridItem(.flexible(), spacing: 8), count: columns), spacing: 8) {
          ForEach(tiles) { tile in
            tileView(tile).frame(height: tileHeight)
          }
        }
        .padding(.horizontal, 12)
        .padding(.top, Self.gridTopRoom)
        .padding(.bottom, Self.gridBottomRoom)
      }
      .scrollBounceBehavior(.basedOnSize)
    }
  }

  private static let gridTopRoom: CGFloat = 136
  private static let gridBottomRoom: CGFloat = 112
  private static let tileShape = RoundedRectangle(cornerRadius: 20, style: .continuous)

  /// Each person once, with their camera when it is on, then cameras nobody could be matched to.
  /// Alone or still connecting: the one being called.
  private var stageTiles: [StageTile] {
    let cameras = orderedStreams.filter { !$0.isScreen }
    let people = call.participantUserIds.map { id in
      StagePerson(
        id: id, name: model.calls.names[id] ?? "",
        isSpeaking: call.speakingUserIds.contains(id),
        isRejoining: call.reconnectingUserIds.contains(id),
        isMuted: call.mutedUserIds.contains(id),
        isUnverified: call.unverifiedUserIds.contains(id),
        hasHandRaised: call.raisedHandUserIds.contains(id))
    }
    var tiles = people.map { person -> StageTile in
      if let stream = cameras.first(where: { call.userIdByDevice[$0.deviceId] == person.id }) {
        return .camera(stream)
      }
      return .avatar(person)
    }
    tiles += cameras.filter { call.userIdByDevice[$0.deviceId] == nil }.map { StageTile.camera($0) }
    if tiles.isEmpty {
      tiles = [.avatar(StagePerson(id: "", name: call.displayName, isSpeaking: false, isRejoining: false, isMuted: false, isUnverified: false))]
    }
    return tiles
  }

  @ViewBuilder
  private func tileView(_ tile: StageTile) -> some View {
    switch tile {
    case .camera(let stream):
      RemoteVideoView(renderer: stream.renderer)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(.black)
        .overlay { videoNotice(for: stream) }
        .overlay(alignment: .bottomLeading) { nameTag(for: stream) }
        .clipShape(Self.tileShape)
        .overlay(Self.tileShape.stroke(isSpeaking(stream) ? Theme.accent : .white.opacity(0.12), lineWidth: isSpeaking(stream) ? 3 : 1))
        .animation(Motion.fast, value: isSpeaking(stream))
    case .avatar(let person):
      GeometryReader { proxy in
        AvatarView(name: person.name, size: min(max(min(proxy.size.width, proxy.size.height) * 0.45, 48), 128))
          .frame(maxWidth: .infinity, maxHeight: .infinity)
      }
      .background(Color(white: 0.14))
      .overlay(alignment: .bottomLeading) { personTag(person) }
      .clipShape(Self.tileShape)
      .overlay(
        Self.tileShape.stroke(person.isSpeaking ? Theme.accent : .white.opacity(0.08), lineWidth: person.isSpeaking ? 3 : 1)
      )
      .opacity(person.isRejoining ? 0.5 : 1)
      .animation(Motion.fast, value: person.isSpeaking)
      .accessibilityElement(children: .ignore)
      .accessibilityLabel(
        CallTileLabel(
          name: handName(person.name, hasHandRaised: person.hasHandRaised), isMuted: person.isMuted,
          isUnverified: person.isUnverified
        ).text
      )
      .accessibilityValue(person.isSpeaking ? Text("call_tile_speaking") : Text(verbatim: ""))
    }
  }

  /// The name on an avatar tile, as `nameTag` has it on a camera.
  @ViewBuilder
  private func personTag(_ person: StagePerson) -> some View {
    if !person.name.isEmpty {
      HStack(spacing: 4) {
        if person.isUnverified { unverifiedMark }
        if person.isSpeaking { speakingMark }
        if person.hasHandRaised { handMark }
        if person.isMuted { Image(systemName: "mic.slash.fill").font(.app(.caption2)) }
        Text(person.name).lineLimit(1)
        hostBadge(of: person.id)
      }
      .font(.app(.caption).weight(.medium))
      .foregroundStyle(.white)
      .padding(.horizontal, 8)
      .padding(.vertical, 2)
      .background(.black.opacity(0.55), in: Capsule())
      .padding(8)
    }
  }

  /// "Host" or "Co-host" after the name on a tile of a meeting's call (#1720).
  @ViewBuilder
  private func hostBadge(of userId: String) -> some View {
    if let badge = HostBadge(userId: userId, hosts: maybeHosts) {
      Text(badge.label)
        .font(.app(.caption2))
        .foregroundStyle(.white.opacity(0.8))
        .lineLimit(1)
    }
  }

  /// Who speaks, as a shape and not only the accent ring (WCAG 1.4.1, #2281); VoiceOver gets the
  /// state as the tile's value.
  private var speakingMark: some View {
    Image(systemName: "waveform")
      .font(.app(.caption2))
      .accessibilityHidden(true)
  }

  /// The warning mark of a tile whose person could not be verified (an SF Symbol, never an emoji);
  /// the name tag's dark capsule is under it in light and dark mode alike.
  private var unverifiedMark: some View {
    Image(systemName: "exclamationmark.triangle.fill")
      .font(.app(.caption2))
      .foregroundStyle(.orange)
      .accessibilityHidden(true)
  }

  /// Why a tile shows no picture (#1838), over the video as web and desktop have it; the filmstrip's
  /// small tiles show a sign and keep the sentence for VoiceOver.
  @ViewBuilder
  private func videoNotice(for stream: RemoteVideoStream, isCompact: Bool = false) -> some View {
    if let notice = stream.maybeNotice ?? pausedNotice(for: stream) {
      if isCompact {
        Image(systemName: "video.slash.fill")
          .font(.app(.body))
          .foregroundStyle(.white.opacity(0.8))
          .accessibilityLabel(Text(notice))
      } else {
        Text(notice)
          .font(.app(.footnote))
          .multilineTextAlignment(.center)
          .foregroundStyle(.white)
          .padding(.horizontal, 12)
          .padding(.vertical, 8)
          .background(.black.opacity(0.55), in: RoundedRectangle(cornerRadius: 12, style: .continuous))
          .padding(16)
      }
    }
  }

  /// Its sender holds the video back for its voice (#1891): a still picture that isn't a fault.
  private func pausedNotice(for stream: RemoteVideoStream) -> LocalizedStringKey? {
    call.pausedVideoStreamIds.contains(stream.id) ? "call_video_paused_by_publisher" : nil
  }

  private func isUnverified(_ stream: RemoteVideoStream) -> Bool {
    call.userIdByDevice[stream.deviceId].map { call.unverifiedUserIds.contains($0) } ?? false
  }

  private func isMuted(_ stream: RemoteVideoStream) -> Bool {
    call.userIdByDevice[stream.deviceId].map { call.mutedUserIds.contains($0) } ?? false
  }

  private func hasHandRaised(_ stream: RemoteVideoStream) -> Bool {
    call.userIdByDevice[stream.deviceId].map { call.raisedHandUserIds.contains($0) } ?? false
  }

  /// The raised hand on a tile's name (#2324).
  private var handMark: some View {
    Image(systemName: "hand.raised.fill").font(.app(.caption2)).foregroundStyle(.yellow)
  }

  /// "Ada, hand raised" for VoiceOver while the hand is up (#2324).
  private func handName(_ name: String, hasHandRaised: Bool) -> String {
    hasHandRaised ? String(localized: "call_person_hand_raised \(name)") : name
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
            .overlay { videoNotice(for: stream, isCompact: true) }
            .overlay(alignment: .bottomLeading) { nameTag(for: stream) }
            .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
            .overlay(
              RoundedRectangle(cornerRadius: 14, style: .continuous)
                .stroke(Theme.accent, lineWidth: isSpeaking(stream) ? 3 : 0))
            .onTapGesture { withAnimation(spring) { maybeSpotlightId = stream.id } }
            .accessibilityElement(children: .ignore)
            .accessibilityAddTraits(.isButton)
            .accessibilityLabel(
              ownerName(of: stream).map {
                CallTileLabel(name: $0, isMuted: isMuted(stream), isUnverified: isUnverified(stream)).text
              } ?? Text("call_video_camera")
            )
            .accessibilityValue(isSpeaking(stream) ? Text("call_tile_speaking") : Text(verbatim: ""))
            .accessibilityHint(Text("call_video_spotlight"))
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
      HStack(spacing: 4) {
        if isUnverified(stream) { unverifiedMark }
        if isSpeaking(stream) { speakingMark }
        if hasHandRaised(stream) { handMark }
        if isMuted(stream) { Image(systemName: "mic.slash.fill").font(.app(.caption2)) }
        Text(name).lineLimit(1)
        if let userId = call.userIdByDevice[stream.deviceId] { hostBadge(of: userId) }
      }
      .font(.app(.caption).weight(.medium))
      .foregroundStyle(.white)
      .accessibilityElement(children: .ignore)
      .accessibilityLabel(
        CallTileLabel(
          name: handName(name, hasHandRaised: hasHandRaised(stream)), isMuted: isMuted(stream),
          isUnverified: isUnverified(stream)
        ).text)
      .accessibilityValue(isSpeaking(stream) ? Text("call_tile_speaking") : Text(verbatim: ""))
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

  /// What picture-in-picture shows: a shared screen, else the video of whoever talks, else the
  /// first video. `nil` without video to show: an audio call gets no floating window.
  private var pictureInPictureStream: RemoteVideoStream? {
    CallPictureInPictureChoice.stream(
      among: orderedStreams, speaking: call.speakingUserIds, userIdByDevice: call.userIdByDevice)
  }

  // MARK: Chrome

  private var chrome: some View {
    VStack(spacing: 0) {
      if areControlsShown { topBar.transition(chromeTransition(edge: .top)) }
      Spacer(minLength: 0)
      VStack(spacing: 12) {
        recordingNotice
        statusHint
        if let lobby = shownLobby, !lobby.people.isEmpty {
          LobbyBanner(waiting: lobby.people.count) { interact { isLobbyShown = true } }
        }
        notetakerBanner
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
        .font(.app(.title2).weight(.semibold))
        .lineLimit(1)
        .foregroundStyle(.white)
      subtitle
        .font(.app(.subheadline))
        .foregroundStyle(.white.opacity(0.7))
        .accessibilityAddTraits(.updatesFrequently)
      if call.isSecured || call.maybePrivacyCode != nil {
        Button { isEncryptionShown = true } label: {
          // While the notetaker is in, the audio goes to OpenRouter, as desktop and web say (#1290).
          Label(
            LocalizedStringKey(
              maybeNotetaker?.isPresent == true
                ? "call_notetaker_badge" : call.isSecured ? "call_secured" : "call_securing"),
            systemImage: maybeNotetaker?.isPresent == true ? "exclamationmark.triangle.fill" : "lock.fill")
            .font(.app(.footnote).weight(.semibold))
            .foregroundStyle(.white)
            .padding(.horizontal, 12)
            .frame(minHeight: 44)
            .background(.ultraThinMaterial, in: Capsule())
        }
        .accessibilityHint(Text("call_encryption_open"))
      }
    }
    .padding(.horizontal, 64)
    .padding(.top, 8)
    .frame(maxWidth: .infinity)
    .overlay(alignment: .topLeading) { minimiseButton }
    .background(alignment: .top) {
      LinearGradient(colors: [.black.opacity(0.5), .clear], startPoint: .top, endPoint: .bottom)
        .ignoresSafeArea()
    }
  }

  /// Puts the call screen away to use the app while the call goes on (#2066); a ringing call waits
  /// for an answer and can't be put away.
  @ViewBuilder
  private var minimiseButton: some View {
    if call.phase != .ringing {
      Button { model.calls.minimise() } label: {
        Image(systemName: "chevron.down")
          .font(.system(size: 17, weight: .semibold))
          .foregroundStyle(.white)
          .frame(width: 44, height: 44)
          .background(.ultraThinMaterial, in: Circle())
      }
      .buttonStyle(PressScaleStyle())
      .padding(.leading, 12)
      .padding(.top, 4)
      .accessibilityLabel(Text("call_minimise"))
    }
  }

  @ViewBuilder
  private var subtitle: some View {
    if call.isOnHold {
      Text("call_on_hold")
    } else {
      switch call.phase {
      case .ringing: Text("call_ringing")
      case .connecting where call.isAwaitingEncryption: Text("call_getting_ready")
      case .connecting: Text(LocalizedStringKey(call.isSecured ? "call_connecting" : "call_securing"))
      case .reconnecting: Text("call_reconnecting")
      case .connected:
        HStack(spacing: 4) {
          if let start = call.maybeConnectedAt {
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

  /// Who records the call (#216, e2ee.md §8.3), from their signed notices: shown for as long as
  /// anybody records, and it can't be closed. Objecting means leaving the call.
  @ViewBuilder
  private var recordingNotice: some View {
    if !call.recordingUserIds.isEmpty {
      HStack(spacing: 8) {
        Circle().fill(.red).frame(width: 8, height: 8).accessibilityHidden(true)
        Text("call_recording_by \(recordingNames)")
      }
      .font(.app(.subheadline).weight(.medium))
      .foregroundStyle(.white)
      .padding(.horizontal, 16)
      .padding(.vertical, 8)
      .background(.black.opacity(0.55), in: Capsule())
      .accessibilityElement(children: .combine)
    }
  }

  /// The people recording the call, by name, for the notice and its announcement.
  private var recordingNames: String {
    let someone = String(localized: "call_recording_someone")
    return call.recordingUserIds
      .map { id in model.calls.names[id].flatMap { $0.isEmpty ? nil : $0 } ?? someone }
      .sorted()
      .joined(separator: ", ")
  }

  /// What matters while talking: that the connection is gone, that nobody hears you, that the own
  /// video waits so the voice gets through (#1902), and that the call runs on the fallback for
  /// networks that block UDP (as on Android, desktop and web).
  @ViewBuilder
  private var statusHint: some View {
    if call.phase == .reconnecting {
      hint("call_reconnecting")
    } else if call.isMuted && call.phase == .connected {
      hint("call_you_muted")
    } else if call.isVideoHeldForVoice && call.phase == .connected {
      hint("call_video_held_for_voice")
    } else if call.isVideoYieldedForVoice && call.phase == .connected {
      hint("call_video_yielded_for_voice")
    } else if call.isOnLimitedNetwork && call.phase == .connected {
      hint("call_limited_network")
    }
  }

  private func hint(_ key: LocalizedStringKey) -> some View {
    Text(key)
      .font(.app(.subheadline).weight(.medium))
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
      if extraControls > 0 || model.calls.isCameraOn {
        // Together in one menu with Chat and the reactions, so the bar keeps its room on small
        // phones (six controls at most).
        Spacer(minLength: 0)
        Menu {
          Button(action: openChat) {
            Label(chatLabel, systemImage: "bubble.left.and.bubble.right")
          }
          Menu {
            reactionItems
          } label: {
            Label("call_reactions", systemImage: "face.smiling")
          }
          handItem
          if isAddingPeopleOffered {
            Button("call_add_people", systemImage: "person.badge.plus") { interact { isAddingPeople = true } }
          }
          if isNotetakerControlShown {
            Button("call_notetaker_add", systemImage: "note.text.badge.plus") { interact { pressNotetaker() } }
          }
          if isHostsOffered {
            Button("call_hosts_title", systemImage: "person.2.badge.gearshape") { interact { isHostsShown = true } }
          }
          if isLobbyOffered {
            Button("call_lobby_title", systemImage: "person.badge.clock") { interact { isLobbyShown = true } }
          }
        } label: {
          Image(systemName: "ellipsis")
            .font(.app(.title2))
            .foregroundStyle(.white)
            .frame(width: Self.controlSize, height: Self.controlSize)
            .background(Color.white.opacity(0.16), in: Circle())
            .contentShape(Circle())
            // With Chat in the menu, a dot says something there is unread.
            .overlay(alignment: .topTrailing) { ChatBadgeView(badge: chatBadge, isDot: true) }
        }
        .accessibilityLabel(Text("more"))
        .accessibilityValue(chatBadge.count > 0 ? Text("chat_unread \(chatBadge.count)") : Text(verbatim: ""))
      } else {
        Spacer(minLength: 0)
        reactionsControl
        Spacer(minLength: 0)
        RoundControl(system: "bubble.left.and.bubble.right.fill", isActive: false, label: chatLabel, action: openChat)
          .overlay(alignment: .topTrailing) { ChatBadgeView(badge: chatBadge, isDot: false) }
      }
      Spacer(minLength: 0)
      hangUpControl
        .disabled(isCheckingLeave || isEnding)
      Spacer(minLength: 0)
    }
    .padding(.vertical, 16)
    .background(alignment: .bottom) {
      LinearGradient(colors: [.clear, .black.opacity(0.55)], startPoint: .top, endPoint: .bottom)
        .ignoresSafeArea()
    }
  }

  /// The reactions and the raised hand as a control of their own (#2324), where the bar has room.
  private var reactionsControl: some View {
    Menu {
      reactionItems
      Divider()
      handItem
    } label: {
      Image(systemName: call.isHandRaised ? "hand.raised.fill" : "face.smiling")
        .font(.app(.title2))
        .foregroundStyle(.white)
        .frame(width: Self.controlSize, height: Self.controlSize)
        .background(Color.white.opacity(call.isHandRaised ? 0.32 : 0.16), in: Circle())
        .contentShape(Circle())
    }
    .accessibilityLabel(Text("call_reactions"))
    .accessibilityValue(call.isHandRaised ? Text("call_hand_raised_value") : Text(verbatim: ""))
  }

  /// One item per reaction the core offers; the emoji is the reaction (user content), the text
  /// its name.
  @ViewBuilder
  private var reactionItems: some View {
    ForEach(callReactions(), id: \.reaction) { choice in
      Button {
        interact { react(choice) }
      } label: {
        Label {
          Text(verbatim: FloatingReactions.name(of: choice.reaction))
        } icon: {
          Text(verbatim: choice.emoji)
        }
      }
    }
  }

  private var handItem: some View {
    let title: LocalizedStringKey = call.isHandRaised ? "call_hand_lower" : "call_hand_raise"
    return Button(title, systemImage: call.isHandRaised ? "hand.raised.slash" : "hand.raised") {
      interact { model.calls.setHandRaised(!call.isHandRaised) }
    }
  }

  /// "Chat", or "Chat, 3 unread messages" for VoiceOver.
  private var chatLabel: LocalizedStringKey {
    chatBadge.count > 0 ? "call_chat_unread \(chatBadge.count)" : "call_chat"
  }

  /// The call's chat (#1488): its conversation in the app, the call going on behind the app's bar
  /// as after "Minimise". The conversation's own feed and composer are the chat, as on desktop
  /// (#936), so reading it there marks it read.
  private func openChat() {
    model.openConversationId = call.conversationId
    model.calls.minimise()
  }

  /// The badge from the conversation list's read: unread messages, loud for a mention or a DM, as
  /// on desktop. Nothing while the store can't be read.
  private func readChatBadge() async {
    guard let item = await model.conversations().first(where: { $0.id == call.conversationId }) else {
      chatBadge = .none
      return
    }
    let isDirectLike = item.kind == .direct || item.kind == .group
    chatBadge = CallChatBadge(
      count: Int(max(item.unreadCount, item.mentionCount)), isLoud: item.mentionCount > 0 || isDirectLike)
  }

  /// How many of "Add people", "Add notetaker", "Hosts" and "Lobby" the bar offers.
  private var extraControls: Int {
    [isAddingPeopleOffered, isNotetakerControlShown, isHostsOffered, isLobbyOffered].filter { $0 }.count
  }

  /// The lobby while connected: only its hosts get one from the core (#1859).
  private var shownLobby: MeetingLobby? {
    call.phase == .connected ? maybeLobby : nil
  }

  /// "Lobby" (#1859): the hosts of the call's meeting see who waits and admit or deny them.
  private var isLobbyOffered: Bool { shownLobby != nil }

  /// Reads the lobby again; a guest's key code the core is still making comes a few reads later.
  private func readLobby() async {
    let conversationId = call.conversationId
    for _ in 0..<8 {
      let lobby = await model.meetingLobby(conversationId: conversationId)
      guard call.conversationId == conversationId, !Task.isCancelled else { return }
      maybeLobby = lobby
      guard let lobby, isWaitingForKeyCode(lobby) else { return }
      try? await Task.sleep(for: .milliseconds(500))
    }
  }

  /// "Hosts" (#1720): the organizer of the call's meeting names co-hosts or hands it over.
  private var isHostsOffered: Bool {
    maybeHosts?.ownPart == .organizer && call.phase == .connected
  }

  private func readHosts() async {
    let conversationId = call.conversationId
    let hosts = await model.meetingHosts(conversationId: conversationId)
    if call.conversationId == conversationId { maybeHosts = hosts }
  }

  /// Hang up. The organizer leaving with people in the lobby and nobody else to let them in is
  /// asked to hand the meeting over first, as on desktop and web; everybody else just leaves.
  private func leave() {
    UIImpactFeedbackGenerator(style: .light).impactOccurred()
    guard let hosts = maybeHosts, hosts.ownPart == .organizer, call.phase == .connected else {
      model.calls.hangUp()
      return
    }
    isCheckingLeave = true
    let conversationId = call.conversationId
    let participants = call.participantUserIds
    Task {
      let maybeOffer = await model.handoverOffer(conversationId: conversationId, participantIds: participants, hosts: hosts)
      isCheckingLeave = false
      if let offer = maybeOffer { maybeHandover = offer } else { model.calls.hangUp() }
    }
  }

  /// Hang up. For the meeting's hosts a long press opens "Leave" and "End for everyone…" (#1930),
  /// as the desktop's split hang-up; a tap leaves as before.
  @ViewBuilder private var hangUpControl: some View {
    if canEndForEveryone {
      Menu {
        Button("call_leave", systemImage: "phone.down") { leave() }
        Button("call_end_for_everyone", systemImage: "xmark.circle", role: .destructive) { isEndAsked = true }
      } label: {
        Image(systemName: "phone.down.fill")
          .font(.app(.title2))
          .foregroundStyle(.white)
          .frame(width: 60, height: 60)
          .background(Theme.danger, in: Circle())
          .contentShape(Circle())
      } primaryAction: {
        leave()
      }
      .menuStyle(.button)
      .buttonStyle(PressScaleStyle())
      .accessibilityLabel(Text("call_hang_up"))
      .accessibilityHint(Text("call_leave_options"))
      .accessibilityAction(named: Text("call_end_for_everyone")) { isEndAsked = true }
    } else {
      RoundControl(system: "phone.down.fill", isActive: false, label: "call_hang_up", tint: Theme.danger) {
        leave()
      }
    }
  }

  /// The organizer or a co-host of the call's meeting, while connected.
  private var canEndForEveryone: Bool {
    guard let hosts = maybeHosts, call.phase == .connected else { return false }
    return hosts.ownPart == .organizer || hosts.ownPart == .coHost
  }

  /// "End for everyone" confirmed: the core ends the meeting, then this device leaves; a refusal
  /// says why and the call goes on.
  private func endForEveryone() {
    guard let meetingId = maybeHosts?.meetingId else { return }
    isEnding = true
    Task {
      let maybeFailure = await model.endMeetingForEveryone(meetingId: meetingId)
      isEnding = false
      if let failure = maybeFailure { maybeEndFailure = failure } else { model.calls.hangUp() }
    }
  }

  private var endFailureBinding: Binding<Bool> {
    Binding(get: { maybeEndFailure != nil }, set: { if !$0 { maybeEndFailure = nil } })
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

  /// "Add people": a DM or group DM call that isn't moving.
  private var isAddingPeopleOffered: Bool {
    canAddPeople && call.phase == .connected && model.calls.maybeMoveOffer == nil && !model.calls.isMoving
  }

  /// "Add notetaker": the policy lets the user ask it in, it isn't in the call or on its way, and
  /// the call isn't moving.
  private var isAddingNotetakerOffered: Bool {
    maybeNotetaker?.canInvite == true && call.phase == .connected && model.calls.maybeMoveOffer == nil
      && !model.calls.isMoving && !model.calls.isAddingNotetaker && !model.calls.isNotetakerAsked
  }

  /// Why the policy doesn't let the user ask the notetaker in, where the button shows disabled
  /// (issue #1290): only while it would otherwise be offered.
  private var notetakerRefusalKey: LocalizedStringKey? {
    guard call.phase == .connected, model.calls.maybeMoveOffer == nil, !model.calls.isMoving else { return nil }
    switch maybeNotetaker?.maybeRefusal {
    case .guest: return "call_notetaker_refused_guest"
    case .adminsOnly: return "call_notetaker_refused_admins"
    case nil: return nil
    }
  }

  private var isNotetakerControlShown: Bool { isAddingNotetakerOffered || notetakerRefusalKey != nil }

  /// "Add notetaker": adds it, or says why the user can't.
  private func pressNotetaker() {
    if isAddingNotetakerOffered {
      addNotetaker()
    } else {
      isNotetakerRefusalShown = true
    }
  }

  private func addNotetaker() {
    guard let notetaker = maybeNotetaker else { return }
    let conversationId = call.conversationId
    Task { await model.calls.addNotetaker(conversationId: conversationId, notetaker: notetaker, requestKey: notetakerKey) }
  }

  /// The notetaker's notice (e2ee.md §8.2, "Everybody sees it"): while it is in the call, where the
  /// audio goes and, for whoever may, "Remove notetaker"; while it was asked in, that it is coming.
  @ViewBuilder
  private var notetakerBanner: some View {
    if let notetaker = maybeNotetaker, notetaker.isPresent {
      VStack(spacing: 12) {
        Label {
          Text("call_notetaker_banner \(notetaker.transcriptionModel)")
        } icon: {
          Image(systemName: "note.text")
        }
        if notetaker.canStop {
          Button("call_notetaker_remove") { model.calls.sendNotetakerAway(conversationId: call.conversationId) }
            .buttonStyle(.bordered)
        }
      }
      .notetakerNotice()
    } else if model.calls.isNotetakerAsked || model.calls.isAddingNotetaker {
      Label {
        Text("call_notetaker_joining")
      } icon: {
        Image(systemName: "note.text")
      }
      .notetakerNotice()
    }
  }

  /// Somebody moved the call into a meeting room: who, whom they added, and the way over.
  private func moveBanner(_ offer: CallMoveOffer) -> some View {
    VStack(spacing: 12) {
      if offer.isNotetakerOnly {
        // Where the audio will go, before the tap that takes the user there.
        Text("call_moved_by \(offer.byName) notetaker")
      } else if offer.addedNames.isEmpty {
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
      Image(systemName: "lock.fill").font(.app(.largeTitle)).foregroundStyle(Theme.accent)
      Text("call_encryption_title").font(.app(.title2).bold())
      Text("call_encryption_body").multilineTextAlignment(.center)
      if let code = call.maybePrivacyCode {
        Text("call_privacy_code_hint")
          .font(.app(.footnote))
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
    Motion.spring(reduceMotion)
  }

  private func chromeTransition(edge: Edge) -> AnyTransition {
    Motion.slide(from: edge, reduceMotion)
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

  private var notetakerFailureBinding: Binding<Bool> {
    Binding(
      get: { model.calls.maybeNotetakerFailure != nil }, set: { if !$0 { model.calls.dismissNotetakerFailure() } })
  }

  private var cameraDeniedBinding: Binding<Bool> {
    Binding(get: { model.calls.isCameraDenied }, set: { if !$0 { model.calls.dismissCameraDenied() } })
  }

  static func format(_ interval: TimeInterval) -> String {
    let seconds = max(0, Int(interval))
    let (hours, minutes, rest) = (seconds / 3600, seconds % 3600 / 60, seconds % 60)
    return hours > 0
      ? String(format: "%d:%02d:%02d", hours, minutes, rest)
      : String(format: "%d:%02d", minutes, rest)
  }
}

/// What a tile of the call's grid shows (#1488).
private enum StageTile: Identifiable {
  case camera(RemoteVideoStream)
  case avatar(StagePerson)

  var id: String {
    switch self {
    case .camera(let stream): return "camera-\(stream.id)"
    case .avatar(let person): return "person-\(person.id)"
    }
  }
}

private struct StagePerson: Identifiable {
  let id: String
  let name: String
  let isSpeaking: Bool
  let isRejoining: Bool
  let isMuted: Bool
  let isUnverified: Bool
  var hasHandRaised = false
}

/// Reads the notetaker again for another call or other people in it.
private struct NotetakerKey: Hashable {
  let conversationId: String
  let participants: [String]
}

/// Reads the Chat button's badge again for another call or after a change in the store.
private struct ChatBadgeKey: Hashable {
  let conversationId: String
  let changes: Int
}

/// The Chat button's badge (#1488): what is unread in the call's conversation, and whether it asks
/// for the user (a mention, a DM), which colours it red, as desktop's.
private struct CallChatBadge: Equatable {
  var count = 0
  var isLoud = false

  static let none = CallChatBadge()
}

/// The unread count on the Chat button, or a dot on "More"; nothing while all is read. The
/// buttons' labels say the count, so the badge itself is hidden from VoiceOver.
private struct ChatBadgeView: View {
  let badge: CallChatBadge
  let isDot: Bool

  var body: some View {
    if badge.count > 0 {
      let fill = badge.isLoud ? Color.red : Color.white
      Group {
        if isDot {
          Circle().fill(fill).frame(width: 10, height: 10)
        } else {
          Text(verbatim: badge.count > 99 ? "99+" : String(badge.count))
            .font(.app(.caption2).weight(.semibold))
            .monospacedDigit()
            .foregroundStyle(badge.isLoud ? Color.white : Color.black)
            .padding(.horizontal, 5)
            .frame(minWidth: 20, minHeight: 20)
            .background(fill, in: Capsule())
        }
      }
      .accessibilityHidden(true)
    }
  }
}

/// Reads the lobby again when the call, its people or a knock change.
private struct LobbyKey: Hashable {
  let conversationId: String
  let participants: [String]
  let knocks: Int
}

extension View {
  /// The look of the notetaker's notice on the call stage.
  fileprivate func notetakerNotice() -> some View {
    self
      .foregroundStyle(.white)
      .multilineTextAlignment(.center)
      .padding()
      .frame(maxWidth: .infinity)
      .background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: 20, style: .continuous))
  }
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
    // A light tap with every press of a call control (#2058). UIKit's generator follows the
    // system's haptics setting: it does nothing where the user turned haptics off.
    Button {
      UIImpactFeedbackGenerator(style: .light).impactOccurred()
      action()
    } label: {
      Image(systemName: system)
        .font(.app(.title2))
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
      let picker = HapticRoutePickerView()
      picker.tintColor = .white
      picker.activeTintColor = .white
      picker.prioritizesVideoDevices = false
      return picker
    }

    func updateUIView(_ view: AVRoutePickerView, context: Context) {}
  }
}

/// The route picker with the light tap of the other call controls (#2058). The picker's own button
/// stays the one that is tapped: a second touch-down action is added to it, so the system's picker
/// opens as before and the tap is not intercepted. The button is found among the subviews once they
/// exist; where the system changes that, the picker simply has no haptic.
private final class HapticRoutePickerView: AVRoutePickerView {
  private var hasHaptic = false

  override func layoutSubviews() {
    super.layoutSubviews()
    guard !hasHaptic, let button = subviews.compactMap({ $0 as? UIButton }).first else { return }
    hasHaptic = true
    button.addAction(
      UIAction { _ in MainActor.assumeIsolated { UIImpactFeedbackGenerator(style: .light).impactOccurred() } }, for: .touchDown)
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

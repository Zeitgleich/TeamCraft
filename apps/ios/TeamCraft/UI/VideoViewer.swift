import AVKit
import Combine
import SwiftUI

/// A video of a message, full screen (#200). The core downloads and decrypts it into the app's
/// private downloads (trimmed at start, wiped with the account), and AVFoundation plays that file.
/// The system's player brings the controls: scrubbing, speed, captions, picture in picture. A format
/// iOS can't decode (WebM, VP8, VP9, AV1 on older chips) says so and offers another app.
struct VideoViewer: View {
  @Environment(AppModel.self) private var model
  @Environment(AppLock.self) private var lock
  @Environment(\.dismiss) private var dismiss
  @Environment(\.scenePhase) private var scenePhase
  let opened: FileInfo

  @State private var maybeFile: FileInfo?
  @State private var phase = VideoPhase.waiting
  /// A tap on "Try again" starts over.
  @State private var attempt = 0
  /// This viewer set the audio session up, so closing it lets go of the session.
  @State private var hasAudioSession = false

  var body: some View {
    ZStack(alignment: .topLeading) {
      Color.black.ignoresSafeArea()
      content.frame(maxWidth: .infinity, maxHeight: .infinity)
      Button {
        dismiss()
      } label: {
        Image(systemName: "xmark").font(.app(.body).weight(.semibold)).frame(width: 44, height: 44)
      }
      .foregroundStyle(.white)
      .accessibilityLabel(Text("picture_close"))
    }
    .task(id: attempt) { await follow() }
    .onDisappear(perform: stop)
    // Nothing of the video stays on screen or in the speakers behind the lock, picture in picture
    // included. The lock's shield only covers the app's own window.
    .onChange(of: lock.isLocked) { _, isLocked in
      guard isLocked else { return }
      clearPlayer()
      dismiss()
    }
    .onChange(of: scenePhase) { _, phase in
      guard case .playing(let player) = self.phase else { return }
      if phase == .background, lock.isEnabled { player.pause() }
      // Back from a start held in the background: play has its sound once the user presses it.
      if phase == .active, !hasAudioSession { startAudioSession() }
    }
    // A call goes first: its screen shows, and the video doesn't talk over it.
    .onChange(of: model.calls.activeCall?.id) { _, maybeCall in
      guard maybeCall != nil else { return }
      clearPlayer()
      dismiss()
    }
  }

  @ViewBuilder
  private var content: some View {
    switch phase {
    case .waiting:
      downloadProgress
    case .playing(let player):
      SystemVideoPlayer(player: player, allowsPictureInPicture: !lock.isEnabled).ignoresSafeArea()
    case .failed:
      notice("file_failed") {
        Button("retry") {
          phase = .waiting
          attempt += 1
        }
      }
    case .gone:
      notice("file_gone") { EmptyView() }
    case .unplayable(let url):
      notice("video_unplayable") {
        ShareLink(item: url) { Text("video_open_elsewhere") }
      }
    }
  }

  /// The poster with the download's progress and the file's size.
  private var downloadProgress: some View {
    let file = maybeFile ?? opened
    return ZStack {
      if let image = ThumbnailCache.image(for: file) {
        Image(uiImage: image).resizable().scaledToFit()
      }
      VStack(spacing: 8) {
        Text("video_loading \(FileSizeText.format(file.sizeBytes))").foregroundStyle(.white)
        if let progress = file.transfer.maybeProgress {
          ProgressView(value: progress).frame(width: 200)
        } else {
          ProgressView().tint(.white)
        }
      }
      .padding()
      .background(Color.black.opacity(0.6), in: RoundedRectangle(cornerRadius: 14))
    }
  }

  private func notice(_ text: LocalizedStringKey, @ViewBuilder action: () -> some View) -> some View {
    VStack(spacing: 12) {
      Text(text).multilineTextAlignment(.center).foregroundStyle(.white)
      action().tint(.white)
    }
    .padding()
    .background(Color.black.opacity(0.6), in: RoundedRectangle(cornerRadius: 14))
    .padding()
  }

  /// Follows the file until it is here, asking for it once, then plays it. Downloads report their end
  /// in the store, not every chunk, so this looks every second while one runs or has failed (a retry
  /// may still be getting through). Cancelled with the view.
  private func follow() async {
    var hasRequested = false
    if attempt > 0 {
      await requestDownload()
      hasRequested = true
    }
    while !Task.isCancelled {
      let extras = await model.extras(conversationId: opened.conversation, messageIds: [opened.message])
      guard let file = extras[opened.message]?.files?.first(where: { $0.fileId == opened.fileId }) else {
        phase = .gone
        return
      }
      maybeFile = file
      switch file.transfer.state {
      case "local":
        if let path = file.transfer.path {
          await play(URL(fileURLWithPath: path), mediaType: file.mediaType)
          return
        }
      case "failed":
        phase = .failed
      case "remote":
        phase = .waiting
        // Opening the video is the request for it: the bubble showed its size and length.
        if !hasRequested {
          hasRequested = true
          await requestDownload()
        }
      default:
        phase = .waiting
        hasRequested = false
      }
      try? await Task.sleep(for: .seconds(1))
    }
  }

  private func requestDownload() async {
    await model.perform(
      .downloadAttachment(conversationId: opened.conversation, messageId: opened.message, fileId: opened.fileId))
  }

  /// Plays the file if iOS can decode its picture, and watches for a failure later on. A video
  /// from another member may refer to other files or addresses: a playlist isn't opened at all, and
  /// nothing a movie refers to is loaded. The type is the message's, not one guessed from the
  /// sender's file name.
  private func play(_ url: URL, mediaType: String) async {
    // Opening counts as use: the downloads are trimmed least recently used first.
    try? FileManager.default.setAttributes([.modificationDate: Date()], ofItemAtPath: url.path)
    guard !VideoFileCheck.isPlaylist(at: url, mediaType: mediaType) else {
      phase = .unplayable(url)
      return
    }
    let asset = AVURLAsset(
      url: url,
      options: [
        AVURLAssetReferenceRestrictionsKey: AVAssetReferenceRestrictions.forbidAll.rawValue,
        AVURLAssetOverrideMIMETypeKey: mediaType,
      ])
    guard await canPlay(asset), !Task.isCancelled else {
      phase = .unplayable(url)
      return
    }
    let item = AVPlayerItem(asset: asset)
    let player = AVPlayer(playerItem: item)
    // The decrypted video stays on this device: no AirPlay to whatever is on the network.
    player.allowsExternalPlayback = false
    phase = .playing(player)
    // A download that ended in the background with the lock on doesn't start talking: audio keeps
    // the app running there. It waits, paused, for the user. The application's state, as `scenePhase`
    // read through this captured view can be an old value.
    if !(lock.isEnabled && UIApplication.shared.applicationState == .background) {
      startAudioSession()
      player.play()
    }
    for await status in item.publisher(for: \.status).values where status == .failed {
      player.pause()
      phase = .unplayable(url)
      return
    }
  }

  /// Sound alone isn't the video: a picture track must be decodable too.
  private func canPlay(_ asset: AVURLAsset) async -> Bool {
    do {
      guard try await asset.load(.isPlayable) else { return false }
      let tracks = try await asset.loadTracks(withMediaType: .video)
      for track in tracks {
        if try await track.load(.isDecodable) { return true }
      }
      return tracks.isEmpty
    } catch {
      return false
    }
  }

  /// Sound also with the silent switch on, as a video the user opened should. A running call keeps
  /// its session.
  private func startAudioSession() {
    guard model.calls.activeCall == nil else { return }
    let session = AVAudioSession.sharedInstance()
    try? session.setCategory(.playback, mode: .moviePlayback)
    try? session.setActive(true)
    hasAudioSession = true
  }

  private func clearPlayer() {
    if case .playing(let player) = phase {
      player.pause()
      player.replaceCurrentItem(with: nil)
    }
    phase = .waiting
  }

  private func stop() {
    clearPlayer()
    // A call that started meanwhile owns the session now.
    guard hasAudioSession, model.calls.activeCall == nil else { return }
    try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
  }
}

/// Where a viewer stands.
private enum VideoPhase {
  /// Reading the file's state, or its download is running.
  case waiting
  case playing(AVPlayer)
  case failed
  /// The message or the file is gone.
  case gone
  case unplayable(URL)
}

/// The system's player with its own controls, inside the viewer.
private struct SystemVideoPlayer: UIViewControllerRepresentable {
  let player: AVPlayer
  let allowsPictureInPicture: Bool

  func makeUIViewController(context: Context) -> AVPlayerViewController {
    let controller = AVPlayerViewController()
    updateUIViewController(controller, context: context)
    return controller
  }

  func updateUIViewController(_ controller: AVPlayerViewController, context: Context) {
    controller.player = player
    controller.allowsPictureInPicturePlayback = allowsPictureInPicture
  }
}

/// Whether a "video" is an HLS playlist. AVFoundation would follow its addresses, which tells a
/// stranger's server this device's IP; a sender's file name and the message's type prove nothing,
/// so the file's first bytes count too.
enum VideoFileCheck {
  private static let playlistExtensions: Set<String> = ["m3u", "m3u8"]
  private static let playlistMark = Data("#EXTM3U".utf8)
  private static let byteOrderMark = Data([0xEF, 0xBB, 0xBF])
  /// Enough for a byte order mark, some blank lines and the mark.
  private static let headBytes = 256

  /// Also a file that can't be read: it wouldn't play, and it can't be told apart.
  static func isPlaylist(at url: URL, mediaType: String) -> Bool {
    guard let handle = try? FileHandle(forReadingFrom: url) else { return true }
    defer { try? handle.close() }
    guard let head = try? handle.read(upToCount: headBytes) else { return true }
    return isPlaylist(fileExtension: url.pathExtension, mediaType: mediaType, head: head)
  }

  /// The URL to hand to QuickLook, which plays videos through the same system code; `nil` for a
  /// playlist, whatever type its message gave it.
  static func previewURL(path: String, mediaType: String) -> URL? {
    let url = URL(fileURLWithPath: path)
    return isPlaylist(at: url, mediaType: mediaType) ? nil : url
  }

  static func isPlaylist(fileExtension: String, mediaType: String, head: Data) -> Bool {
    if playlistExtensions.contains(fileExtension.lowercased()) { return true }
    if mediaType.lowercased().contains("mpegurl") { return true }
    var rest = head.starts(with: byteOrderMark) ? head.dropFirst(byteOrderMark.count) : head[...]
    rest = rest.drop { [0x09, 0x0A, 0x0D, 0x20].contains($0) }
    return rest.starts(with: playlistMark)
  }
}

/// A video's length as a badge shows it ("0:07", "12:34", "1:02:03").
enum VideoDurationText {
  static func format(milliseconds: Int64) -> String {
    let total = max(milliseconds, 0) / 1000
    let (hours, minutes, seconds) = (total / 3600, total / 60 % 60, total % 60)
    if hours > 0 { return String(format: "%d:%02d:%02d", hours, minutes, seconds) }
    return String(format: "%d:%02d", minutes, seconds)
  }
}

/// The round play mark over a video's poster.
struct VideoPlayMark: View {
  var side: CGFloat = 48

  var body: some View {
    Image(systemName: "play.fill")
      .font(.system(size: side * 0.42))
      .foregroundStyle(.white)
      .frame(width: side, height: side)
      .background(Color.black.opacity(0.55), in: Circle())
      .accessibilityHidden(true)
  }
}

/// The length and size on a video's poster, known before anything is downloaded.
struct VideoBadge: View {
  let file: FileInfo

  var body: some View {
    let size = FileSizeText.format(file.sizeBytes)
    let text = file.maybeDurationMs.map { "\(VideoDurationText.format(milliseconds: $0)) · \(size)" } ?? size
    Text(verbatim: text)
      .font(.app(.caption2))
      .foregroundStyle(.white)
      .padding(.horizontal, 6).padding(.vertical, 2)
      .background(Color.black.opacity(0.55), in: RoundedRectangle(cornerRadius: 6))
      .padding(6)
  }
}

/// Opens a video in the viewer of the screen around it: on a row of a lazy list a viewer of its own
/// would go when new messages push the row out.
private struct OpenVideoKey: EnvironmentKey {
  static let defaultValue: OpenPictureAction? = nil
}

extension EnvironmentValues {
  var openVideo: OpenPictureAction? {
    get { self[OpenVideoKey.self] }
    set { self[OpenVideoKey.self] = newValue }
  }
}

import QuickLook
import SwiftUI

/// A file in a message: images as a large preview, other files with icon, name and size; progress
/// while it moves. A tap on a picture opens the image viewer where the screen offers it (#240);
/// other files open in Quick Look once downloaded (the core decrypts them into the app's private
/// storage), and are downloaded first.
struct AttachmentView: View {
  @Environment(AppModel.self) private var model
  @Environment(\.openPicture) private var maybeOpenPicture
  @Environment(\.openVideo) private var maybeOpenVideo
  @Environment(\.imageActions) private var maybeImageActions
  @Environment(\.accessibilityReduceMotion) private var isMotionReduced
  @Environment(\.areAnimationsCovered) private var areAnimationsCovered
  @Environment(\.scenePhase) private var scenePhase
  @Environment(\.displayScale) private var displayScale
  @AppStorage(AnimatedImagesSetting.key) private var animatedImages = AnimatedImagesSetting.fallback.rawValue
  let file: FileInfo
  /// A tile of an album, sized by the album, instead of the large preview.
  var isTile = false
  /// On the album's last tile: how many pictures the album doesn't show ("+3").
  var maybeHiddenCount: Int?
  /// In a chat bubble: a picture spans this width at its own shape, and a file row takes the
  /// bubble's colours (`isInBubble`).
  var maybeMediaWidth: CGFloat?
  var isInBubble = false

  @State private var maybePreviewURL: URL?
  @State private var isPreviewRefused = false
  @State private var isWaitingToOpen = false
  @State private var animation = AnimationPlayer()
  @State private var playChoice = PlayChoice.unset
  @State private var maybeVideo: FileInfo?

  var body: some View {
    withImageMenu(content)
  }

  /// A tile of an album has its own menu on a long press (#241); everything else keeps the message's.
  @ViewBuilder
  private func withImageMenu(_ view: some View) -> some View {
    if isTile, let actions = maybeImageActions {
      view.contextMenu {
        if let reply = actions.maybeReply {
          Button("image_reply", systemImage: "arrowshape.turn.up.left") { reply(file) }
        }
        Button("image_forward_one", systemImage: "arrowshape.turn.up.right") { actions.forward(file) }
        if file.transfer.state == "local", let path = file.transfer.path {
          ShareLink(item: URL(fileURLWithPath: path)) { Label("picture_share", systemImage: "square.and.arrow.up") }
        } else {
          Button("file_download", systemImage: "arrow.down.circle") {
            Task {
              await model.perform(
                .downloadAttachment(conversationId: file.conversation, messageId: file.message, fileId: file.fileId))
            }
          }
        }
        Button("open", systemImage: "eye", action: open)
      }
    } else {
      view
    }
  }

  private var content: some View {
    VStack(alignment: .leading, spacing: 2) {
      if file.isVoiceMessage {
        // A voice message plays in place (#204); the message's menu still downloads or forwards it.
        VoiceCard(file: file, isInBubble: isInBubble)
      } else {
        mediaButton
      }
    }
    .quickLookPreview($maybePreviewURL)
    .alert("video_unplayable", isPresented: $isPreviewRefused) { Button("done", role: .cancel) {} }
    .fullScreenCover(item: $maybeVideo) { video in VideoViewer(opened: video) }
    .onChange(of: file.transfer.state) { _, state in
      guard isWaitingToOpen, state == "local" || state == "failed" else { return }
      isWaitingToOpen = false
      if state == "local", let path = file.transfer.path { preview(path) }
    }
  }

  private var mediaButton: some View {
    Button(action: open) {
      if file.isImage, let image = ThumbnailCache.image(for: file) {
        if isTile {
          tile(image)
        } else if let width = maybeMediaWidth {
          bubblePreview(image, width: width)
        } else {
          imagePreview(image)
        }
      } else if file.isVideo, let image = ThumbnailCache.image(for: file), let width = maybeMediaWidth {
        bubblePreview(image, width: width)
          .overlay { VideoPlayMark() }
          .overlay(alignment: .bottomLeading) { VideoBadge(file: file) }
      } else {
        fileRow
      }
    }
    .buttonStyle(.plain)
    .accessibilityLabel(Text(file.fileName))
    .accessibilityValue(maybeHiddenCount.map { Text("album_more_pictures \($0)") } ?? Text(verbatim: ""))
    // Beside the picture's button, not in it: a tap here stops or starts it (#201), a tap on the
    // rest opens the viewer as on any picture.
    .overlay(alignment: .topLeading) {
      if canMoveHere { AnimationButton(isMoving: isGoing, action: toggleMotion) }
    }
    .overlay(alignment: .bottomLeading) {
      if canMoveHere && !isMoving {
        AnimationBadge(
          text: file.transfer.state == "failed" ? Text("animation_failed") : Text(verbatim: AnimationKind.label(file.mediaType)))
      }
    }
    .task(id: motionTask) { await runMotion() }
  }

  // MARK: Animated images (#201)

  private var mayMove: Bool { file.isImage && AnimationKind.mayMove(file.mediaType) && !isPlainSurface }

  /// Pictures in the plain preview (search, files) stay still: only the conversation's tiles move.
  private var isPlainSurface: Bool { !isTile && maybeMediaWidth == nil }

  private var localPath: String? { file.transfer.state == "local" ? file.transfer.path : nil }

  private var canMoveHere: Bool {
    mayMove && (AnimationKind.isAnimation(file.mediaType) || animation.maybeMoves == true) && animation.maybeMoves != false
  }

  private var wantsToMove: Bool {
    playChoice.wantsToMove(AnimatedImagesSetting.stored(animatedImages).effective(isMotionReduced: isMotionReduced))
  }

  /// It moves right now, or will as soon as its file is here.
  private var isGoing: Bool { isMoving || (wantsToMove && file.transfer.state == "downloading") }

  private var isVisible: Bool { scenePhase == .active && !areAnimationsCovered }

  private var isMoving: Bool { wantsToMove && isVisible && localPath != nil }

  /// What `runMotion` depends on; the running task ends when it changes.
  private var motionTask: MotionTask {
    MotionTask(mayMove: mayMove, state: file.transfer.state, path: localPath, isMoving: isMoving, wantsToMove: wantsToMove)
  }

  private struct MotionTask: Equatable {
    let mayMove: Bool
    let state: String
    let path: String?
    let isMoving: Bool
    let wantsToMove: Bool
  }

  private func toggleMotion() {
    let wasGoing = isGoing
    playChoice = wasGoing ? .paused : .playing
    // Asked for by hand: on any network and at any size.
    if !wasGoing, file.transfer.state == "remote" || file.transfer.state == "failed" { fetchFile() }
  }

  private func fetchFile() {
    Task {
      await model.perform(
        .downloadAttachment(conversationId: file.conversation, messageId: file.message, fileId: file.fileId))
    }
  }

  /// `.task` runs this while the tile is on screen and cancels it when the tile leaves.
  private func runMotion() async {
    guard mayMove else { return }
    guard let path = localPath else {
      guard AnimationKind.isAnimation(file.mediaType), wantsToMove, isVisible, playChoice == .unset,
        file.sizeBytes <= AnimationKind.maxAutomaticBytes, file.transfer.state == "remote"
      else { return }
      // Cancelled when the tile leaves, so a tile scrolled past fetches nothing; a fetch once started
      // runs on.
      try? await Task.sleep(for: AnimationKind.fetchSettle)
      guard !Task.isCancelled,
        fetchesOriginalAutomatically(
          setting: OriginalDownloadSetting.current.setting, isMetered: NetworkMonitor.shared.isMetered),
        AutoFetchBudget.shared.take(file.sizeBytes)
      else { return }
      fetchFile()
      return
    }
    if isMoving {
      await animation.run(path: path, maxPixels: tileMaxPixels)
      if animation.hasEnded { playChoice = .paused }
    } else if animation.maybeMoves == nil {
      await animation.probe(path: path)
    }
  }

  private var tileMaxPixels: CGFloat { (maybeMediaWidth ?? 240) * displayScale }

  private func tile(_ image: UIImage) -> some View {
    Color.clear
      .overlay { Image(uiImage: animation.maybeFrame ?? image).resizable().scaledToFill() }
      .overlay {
        if let hidden = maybeHiddenCount {
          // The pictures beyond the album's tiles; they open in the viewer.
          Text(verbatim: "+\(hidden)")
            .font(.app(.title2).weight(.semibold))
            .foregroundStyle(.white)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .background(Color.black.opacity(0.45))
        }
      }
      .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
      .overlay(alignment: .bottom) {
        if file.transfer.state == "downloading", let progress = file.transfer.maybeProgress {
          ProgressView(value: progress).padding(4)
        }
      }
      .contentShape(Rectangle())
  }

  /// A picture across the bubble at its own shape, at most `bubbleMediaMaxHeight` tall: panoramas
  /// and tall screenshots are cut to a sensible shape rather than shrunk to a line.
  private func bubblePreview(_ image: UIImage, width: CGFloat) -> some View {
    let size = bubblePictureSize(image, room: width)
    return Color.clear
      .frame(width: size.width, height: size.height)
      .overlay { Image(uiImage: animation.maybeFrame ?? image).resizable().scaledToFill() }
      .clipShape(RoundedRectangle(cornerRadius: bubbleMediaCornerRadius, style: .continuous))
      .overlay(alignment: .bottom) {
        if file.transfer.state == "downloading", let progress = file.transfer.maybeProgress {
          ProgressView(value: progress).padding(6)
        }
      }
      .overlay(alignment: .bottomLeading) {
        if file.transfer.state == "failed" || file.transfer.state == "upload_failed" {
          FileStateLine(file: file)
            .padding(.horizontal, 8).padding(.vertical, 4)
            .background(.regularMaterial, in: Capsule())
            .padding(6)
        }
      }
  }

  private func imagePreview(_ image: UIImage) -> some View {
    VStack(alignment: .leading, spacing: 4) {
      Image(uiImage: image).resizable().scaledToFit()
        .frame(maxWidth: 260, maxHeight: 220, alignment: .leading)
        .clipShape(RoundedRectangle(cornerRadius: Theme.radius))
      FileStateLine(file: file)
    }
  }

  private var fileRow: some View {
    HStack(spacing: 10) {
      FileThumbnail(file: file, side: 56)
      VStack(alignment: .leading, spacing: 2) {
        Text(file.fileName).font(.app(.subheadline).weight(.semibold)).lineLimit(2)
        Text(FileSizeText.format(file.sizeBytes)).font(.app(.caption))
          .foregroundStyle(isInBubble ? AnyShapeStyle(HierarchicalShapeStyle.secondary) : AnyShapeStyle(Theme.textMuted))
        FileStateLine(file: file)
      }
      if !isInBubble { Spacer(minLength: 0) }
    }
    .padding(8)
    .background(rowBackground)
    .overlay(RoundedRectangle(cornerRadius: Theme.radius).stroke(isInBubble ? Color.clear : Theme.border))
    .clipShape(RoundedRectangle(cornerRadius: Theme.radius))
  }

  private var rowBackground: AnyShapeStyle {
    isInBubble ? AnyShapeStyle(HierarchicalShapeStyle.primary.opacity(0.08)) : AnyShapeStyle(Theme.surface)
  }

  private func preview(_ path: String) {
    DownloadUse.mark(path)
    maybePreviewURL = VideoFileCheck.previewURL(path: path, mediaType: file.mediaType)
    isPreviewRefused = maybePreviewURL == nil
  }

  private func open() {
    if file.isViewablePicture, let openPicture = maybeOpenPicture {
      openPicture(file)
      return
    }
    if file.opensInVideoPlayer {
      if let openVideo = maybeOpenVideo { openVideo(file) } else { maybeVideo = file }
      return
    }
    switch file.transfer.state {
    case "local":
      if let path = file.transfer.path { preview(path) }
    case "remote", "failed":
      isWaitingToOpen = true
      Task { await model.perform(.downloadFile(conversationId: file.conversation, messageId: file.message)) }
    default:
      break
    }
  }
}

/// A download's progress, a failure or the hint to tap, under a file's name; an upload's progress is
/// the message's one `UploadControl`.
struct FileStateLine: View {
  let file: FileInfo

  var body: some View {
    switch file.transfer.state {
    case "downloading":
      VStack(alignment: .leading, spacing: 2) {
        if let progress = file.transfer.maybeProgress { ProgressView(value: progress) } else { ProgressView() }
        Text("file_downloading").font(.app(.caption2)).foregroundStyle(Theme.textMuted)
      }
    case "failed", "upload_failed":
      Text("file_failed").font(.app(.caption2)).foregroundStyle(Theme.danger)
    case "remote":
      Text("file_tap_to_download").font(.app(.caption2)).foregroundStyle(Theme.textMuted)
    default:
      EmptyView()
    }
  }
}

/// The one control of a message's upload, as on Android, desktop and web: a ring for all its files
/// with a stop square that cancels them, and "Processing…" or "3.2 MB / 70 MB" next to it; "Try
/// again" once it stopped. Over pictures a translucent pill in their corner, under documents a row.
struct UploadControl: View {
  @Environment(AppModel.self) private var model
  let files: [FileInfo]
  let pill: UploadPill
  var isOnMedia = true

  var body: some View {
    content
      .font(.app(.caption).weight(.semibold))
      .monospacedDigit()
      .lineLimit(1)
      .padding(.leading, 2)
      .padding(.trailing, isFailed ? 2 : 12)
      .frame(minHeight: 36)
      .foregroundStyle(isOnMedia ? Color.white : Color.primary)
      .background {
        if isOnMedia {
          Capsule().fill(.ultraThinMaterial).environment(\.colorScheme, .dark)
        } else {
          Capsule().fill(Color.primary.opacity(0.1))
        }
      }
      .padding(isOnMedia ? 6 : 0)
      .animation(.snappy, value: isFailed)
  }

  @ViewBuilder
  private var content: some View {
    switch pill {
    case .failed(let isStorageUnavailable):
      HStack(spacing: 0) {
        Button(action: retry) {
          Label("retry", systemImage: "arrow.up").padding(.horizontal, 8).frame(minHeight: 32)
        }
        .accessibilityHint(Text(isStorageUnavailable ? "file_upload_storage_unavailable" : "file_upload_stalled"))
        Button(action: cancel) {
          Image(systemName: "xmark").font(.app(.caption2).weight(.bold)).frame(width: 32, height: 32)
        }
        .accessibilityLabel(Text("file_cancel_upload"))
      }
      .buttonStyle(.plain)
    default:
      HStack(spacing: 4) {
        Button(action: cancel) { UploadRing(maybeFraction: pill.maybeFraction) }
          .buttonStyle(.plain)
          .accessibilityLabel(Text("file_cancel_upload"))
        label
      }
    }
  }

  private var isFailed: Bool {
    if case .failed = pill { return true }
    return false
  }

  @ViewBuilder
  private var label: some View {
    switch pill {
    case .processing: Text("file_upload_processing")
    case .retrying: Text("file_upload_retrying")
    case .sending(let done, let total):
      Text("file_upload_progress \(FileSizeText.format(done)) \(FileSizeText.format(total))")
    case .failed: EmptyView()
    }
  }

  private func cancel() {
    let pending = UploadPill.cancellable(files)
    Task {
      // One after another, last first: the core finds a pending file by its place.
      for file in pending {
        await model.perform(
          .cancelUpload(conversationId: file.conversation, messageId: file.message, fileId: file.fileId))
      }
    }
  }

  private func retry() {
    guard let file = files.first else { return }
    Task { await model.perform(.retrySend(conversationId: file.conversation, messageId: file.message)) }
  }
}

/// An upload's ring with a stop square in the middle: filled to how far the files got, or a quarter
/// arc that turns before the first byte. It eases to each value; with Reduce Motion it doesn't move.
struct UploadRing: View {
  @Environment(\.accessibilityReduceMotion) private var isMotionReduced
  let maybeFraction: Double?
  @State private var isTurning = false

  var body: some View {
    ZStack {
      Circle().stroke(lineWidth: 2.5).opacity(0.3)
      if let fraction = maybeFraction {
        Circle()
          .trim(from: 0, to: fraction)
          .stroke(style: StrokeStyle(lineWidth: 2.5, lineCap: .round))
          .rotationEffect(.degrees(-90))
          .animation(isMotionReduced ? nil : .easeOut(duration: 0.3), value: fraction)
      } else {
        Circle()
          .trim(from: 0, to: 0.25)
          .stroke(style: StrokeStyle(lineWidth: 2.5, lineCap: .round))
          .rotationEffect(.degrees(isTurning ? 270 : -90))
          .animation(isMotionReduced ? nil : .linear(duration: 1).repeatForever(autoreverses: false), value: isTurning)
          .onAppear { isTurning = true }
      }
      RoundedRectangle(cornerRadius: 2, style: .continuous).frame(width: 9, height: 9)
    }
    .frame(width: 26, height: 26)
    .frame(width: 32, height: 32)
    .contentShape(Circle())
  }
}

/// The sender's thumbnail, or an icon by file kind.
struct FileThumbnail: View {
  let file: FileInfo
  let side: CGFloat

  var body: some View {
    Group {
      if let image = ThumbnailCache.image(for: file) {
        Image(uiImage: image).resizable().scaledToFill()
      } else {
        Image(systemName: icon).font(.app(.title2)).foregroundStyle(Theme.textMuted)
          .frame(maxWidth: .infinity, maxHeight: .infinity).background(Theme.accentSoft)
      }
    }
    .frame(width: side, height: side)
    .clipShape(RoundedRectangle(cornerRadius: 8))
    .overlay {
      if file.isVideo, file.maybeThumbnailData != nil { VideoPlayMark(side: side * 0.4) }
    }
  }

  private var icon: String {
    if file.isImage { return "photo" }
    if file.mediaType.hasPrefix("video/") { return "film" }
    if file.mediaType.hasPrefix("audio/") { return "waveform" }
    return "doc"
  }
}

/// Sizes the way the system writes them ("1.2 MB").
enum FileSizeText {
  static func format(_ bytes: Int64) -> String {
    ByteCountFormatter.string(fromByteCount: bytes, countStyle: .file)
  }
}

/// The tallest a picture in a bubble gets; its width is the bubble's.
let bubbleMediaMaxHeight: CGFloat = 340
let bubbleMediaCornerRadius: CGFloat = 14

/// A picture's size in a bubble: its own shape (panoramas and tall screenshots cut to a sensible
/// one), as wide as `room` allows and at most `bubbleMediaMaxHeight` tall.
func bubblePictureSize(_ image: UIImage, room: CGFloat) -> CGSize {
  let ratio = image.size.height > 0 ? image.size.width / image.size.height : 4 / 3
  let shape = min(max(ratio, 0.6), 2.2)
  let width = min(room, bubbleMediaMaxHeight * shape)
  return CGSize(width: width, height: width / shape)
}

/// Decoded thumbnails by file, so scrolling past a picture doesn't decode it again on every render.
enum ThumbnailCache {
  private static let images: NSCache<NSString, UIImage> = {
    let cache = NSCache<NSString, UIImage>()
    cache.countLimit = 200
    return cache
  }()

  static func image(for file: FileInfo) -> UIImage? {
    let key = file.fileId as NSString
    if let cached = images.object(forKey: key) { return cached }
    guard let data = file.maybeThumbnailData, let image = UIImage(data: data) else { return nil }
    images.setObject(image, forKey: key)
    return image
  }

  /// The copy a reply carries; another picture than the file's may stand under the same ID.
  static func image(forQuote quoted: ReplyAttachmentPreview) -> UIImage? {
    let key = "quote:\(quoted.fileId)" as NSString
    if let cached = images.object(forKey: key) { return cached }
    guard let data = quoted.thumbnailData, let image = UIImage(data: data) else { return nil }
    images.setObject(image, forKey: key)
    return image
  }
}

/// A link preview card from the data the sender's core fetched.
struct LinkPreviewCard: View {
  let preview: LinkPreview
  /// In a chat bubble: tinted with the bubble's text colour instead of a card of its own.
  var isInBubble = false

  var body: some View {
    let content = VStack(alignment: .leading, spacing: 4) {
      if let data = preview.maybeImageData, let image = UIImage(data: data) {
        Image(uiImage: image).resizable().scaledToFit().frame(maxHeight: 160)
          .clipShape(RoundedRectangle(cornerRadius: 8))
      }
      if let site = preview.maybeSiteName {
        Text(site).font(.app(.caption)).foregroundStyle(muted)
      }
      if let title = preview.maybeTitle {
        Text(title).font(.app(.subheadline).weight(.semibold)).lineLimit(2)
      }
      if let description = preview.maybeDescription {
        Text(description).font(.app(.caption)).foregroundStyle(muted).lineLimit(3)
      }
    }
    .frame(maxWidth: .infinity, alignment: .leading)
    .padding(10)
    .padding(.leading, 3)
    .background(isInBubble ? AnyShapeStyle(HierarchicalShapeStyle.primary.opacity(0.08)) : AnyShapeStyle(Theme.surface))
    .overlay(alignment: .leading) {
      Rectangle().fill(isInBubble ? AnyShapeStyle(HierarchicalShapeStyle.primary) : AnyShapeStyle(Theme.accent)).frame(width: 3)
    }
    .clipShape(RoundedRectangle(cornerRadius: 8))
    if let url = preview.maybeOpenableURL {
      Link(destination: url) { content }.buttonStyle(.plain)
    } else {
      content
    }
  }

  private var muted: AnyShapeStyle {
    isInBubble ? AnyShapeStyle(HierarchicalShapeStyle.secondary) : AnyShapeStyle(Theme.textMuted)
  }
}

/// Every file of a conversation as a gallery; tap opens or downloads.
struct FilesGalleryView: View {
  /// Whose files it shows.
  enum Source: Hashable {
    case conversation(String)
    case space(String)
  }

  @Environment(AppModel.self) private var model
  let source: Source

  @State private var files: [FileInfo]?
  @State private var maybePreviewURL: URL?
  @State private var isPreviewRefused = false
  @State private var maybeViewing: PictureTarget?
  @State private var maybeVideo: FileInfo?

  private let columns = [GridItem(.adaptive(minimum: 110), spacing: 8)]

  init(conversation: ConversationItem) {
    source = .conversation(conversation.id)
  }

  init(spaceId: String) {
    source = .space(spaceId)
  }

  var body: some View {
    Group {
      if let files, files.isEmpty {
        Text(emptyText).foregroundStyle(Theme.textMuted)
      } else {
        ScrollView {
          LazyVGrid(columns: columns, spacing: 8) {
            ForEach(files ?? []) { file in
              tile(file)
            }
          }
          .padding()
        }
      }
    }
    .frame(maxWidth: .infinity, maxHeight: .infinity)
    .background(Theme.background)
    .navigationTitle("files_title")
    .navigationBarTitleDisplayMode(.inline)
    .task(id: LoadKey(changeCount: model.changeCount, source: source)) { files = await load() }
    .quickLookPreview($maybePreviewURL)
    .alert("video_unplayable", isPresented: $isPreviewRefused) { Button("done", role: .cancel) {} }
    .fullScreenCover(item: $maybeViewing) { target in PictureViewer(target: target) }
    .fullScreenCover(item: $maybeVideo) { video in VideoViewer(opened: video) }
  }

  private struct LoadKey: Hashable {
    let changeCount: Int
    let source: Source
  }

  private var emptyText: LocalizedStringKey {
    switch source {
    case .conversation: return "files_empty"
    case .space: return "space_files_empty"
    }
  }

  private func load() async -> [FileInfo] {
    switch source {
    case .conversation(let conversation): return await model.files(conversationId: conversation)
    case .space(let space): return await model.files(spaceId: space)
    }
  }

  private func tile(_ file: FileInfo) -> some View {
    Button {
      if file.isViewablePicture {
        maybeViewing = PictureTarget(file: file)
      } else if file.opensInVideoPlayer {
        maybeVideo = file
      } else if file.transfer.state == "local", let path = file.transfer.path {
        DownloadUse.mark(path)
        maybePreviewURL = VideoFileCheck.previewURL(path: path, mediaType: file.mediaType)
        isPreviewRefused = maybePreviewURL == nil
      } else if file.transfer.state == "remote" || file.transfer.state == "failed" {
        Task { await model.perform(.downloadFile(conversationId: file.conversation, messageId: file.message)) }
      }
    } label: {
      VStack(alignment: .leading, spacing: 4) {
        FileThumbnail(file: file, side: 110)
        Text(file.fileName).font(.app(.caption)).lineLimit(1)
        Text(FileSizeText.format(file.sizeBytes)).font(.app(.caption2)).foregroundStyle(Theme.textMuted)
        FileStateLine(file: file)
      }
      .frame(width: 110, alignment: .leading)
    }
    .buttonStyle(.plain)
  }
}

import SwiftUI
import UIKit

/// The in-app image viewer (#240), over the conversation: the tapped picture at once from its
/// thumbnail, the original swapped in when it is here (fetched by itself or on "Load original",
/// Settings → Chats), with its progress, or "Try again" when it failed. Swipe through the pictures of
/// the message, or of the whole conversation; the ends don't wrap around. Pinch, double tap and pan
/// to zoom (the same limits as on every platform); a tap shows or hides the bars. Sender and time on
/// top; share and forward at the bottom.
struct PictureViewer: View {
  @Environment(AppModel.self) private var app
  @Environment(\.dismiss) private var dismiss
  @Environment(\.displayScale) private var displayScale
  @State private var model: PictureViewerModel
  @State private var isChromeShown = true
  /// The page that is zoomed in: it pans instead of turning the page.
  @State private var maybeZoomedKey: String?
  @State private var maybeForwarding: ForwardTarget?
  @State private var displayNames: [String: String] = [:]

  init(target: PictureTarget) {
    _model = State(initialValue: PictureViewerModel(target: target))
  }

  var body: some View {
    @Bindable var model = model
    ZStack {
      Color.black.ignoresSafeArea()
      if model.items.isEmpty {
        // The opened message is still being read: its thumbnail stands in.
        page(key: model.target.id, file: model.target.file)
      } else {
        pager(position: $model.currentKey)
      }
      if isChromeShown {
        chrome.transition(.opacity)
      }
    }
    .animation(.easeOut(duration: 0.2), value: isChromeShown)
    .environment(\.colorScheme, .dark)
    .statusBarHidden(!isChromeShown)
    .persistentSystemOverlays(isChromeShown ? .automatic : .hidden)
    .background {
      GeometryReader { proxy in
        Color.clear.onAppear {
          model.maxDecodePixels = PictureDecoder.maxPixels(screen: proxy.size, scale: displayScale)
        }
      }
    }
    .task {
      displayNames = await app.displayNames()
      await model.load(app: app)
    }
    .task(id: app.changeCount) { await model.refresh(app: app) }
    .task(id: model.isDownloading) {
      // Download progress doesn't always come as a change: read it while it runs, on screen only.
      while model.isDownloading {
        try? await Task.sleep(for: .seconds(1))
        if Task.isCancelled { return }
        await model.refresh(app: app)
      }
    }
    .onChange(of: model.currentKey) { _, key in
      maybeZoomedKey = nil
      model.show(key)
      Task { await model.load(app: app) }
    }
    .onChange(of: model.isGone) { _, isGone in
      if isGone { dismiss() }
    }
    .sheet(item: $maybeForwarding) { target in
      ForwardSheet(fromConversationId: target.conversationId, messageId: target.messageId)
    }
  }

  private func pager(position: Binding<String?>) -> some View {
    ScrollView(.horizontal) {
      LazyHStack(spacing: 0) {
        ForEach(model.items, id: \.key) { item in
          page(key: item.key, file: model.files[item.key])
            .containerRelativeFrame([.horizontal, .vertical])
            .id(item.key)
        }
      }
      .scrollTargetLayout()
    }
    .scrollTargetBehavior(.paging)
    .scrollPosition(id: position)
    .scrollIndicators(.hidden)
    .scrollDisabled(maybeZoomedKey != nil)
    .ignoresSafeArea()
  }

  /// One picture. Pages outside the window have no file yet: their thumbnail comes as they near.
  private func page(key: String, file maybeFile: FileInfo?) -> some View {
    let state = maybeFile == nil ? OriginalState.loading(nil) : model.originalState(key)
    return PicturePage(
      maybeFile: maybeFile,
      maybeOriginal: model.originals[key],
      state: state,
      maxZoom: model.maxZoom,
      doubleTapZoom: model.doubleTapZoom,
      onTap: { isChromeShown.toggle() },
      onZoomChange: { isZoomed in
        if isZoomed {
          maybeZoomedKey = key
        } else if maybeZoomedKey == key {
          maybeZoomedKey = nil
        }
      },
      onFetch: { Task { await model.fetchOriginal(key, app: app) } })
  }

  /// The bars over the picture, on a dark material so the picture stays visible under them.
  private var chrome: some View {
    VStack(spacing: 0) {
      topBar
      Spacer(minLength: 0)
      bottomBar
    }
  }

  private var topBar: some View {
    HStack(spacing: 4) {
      Button { dismiss() } label: {
        Image(systemName: "xmark").font(.body.weight(.semibold)).frame(width: 44, height: 44)
      }
      .accessibilityLabel(Text("picture_close"))
      VStack(alignment: .leading, spacing: 1) {
        if let name = senderName {
          Text(name).font(.subheadline.weight(.semibold)).lineLimit(1)
        }
        if let date = sentDate {
          Text(date.formatted(date: .abbreviated, time: .shortened)).font(.caption).foregroundStyle(.secondary)
        }
      }
      .accessibilityElement(children: .combine)
      Spacer(minLength: 8)
      if let index = model.currentIndex, model.items.count > 1 {
        Text("picture_position \(index + 1) \(model.items.count)")
          .font(.footnote.monospacedDigit())
          .foregroundStyle(.secondary)
          .padding(.trailing, 12)
      }
    }
    .foregroundStyle(.white)
    .padding(.horizontal, 4)
    .background(.ultraThinMaterial)
  }

  private var bottomBar: some View {
    HStack(spacing: 16) {
      if model.canSwitchScope {
        Picker("picture_scope", selection: scopeBinding) {
          Text("picture_scope_message").tag(MediaScope.album)
          Text("picture_scope_conversation").tag(MediaScope.conversation)
        }
        .pickerStyle(.segmented)
        .fixedSize()
      }
      Spacer(minLength: 0)
      if let url = localURL {
        ShareLink(item: url) {
          Image(systemName: "square.and.arrow.up").frame(width: 44, height: 44)
        }
        .accessibilityLabel(Text("picture_share"))
      }
      if let file = model.currentFile ?? (model.items.isEmpty ? model.target.file : nil) {
        Button {
          maybeForwarding = ForwardTarget(conversationId: file.conversation, messageId: file.message)
        } label: {
          Image(systemName: "arrowshape.turn.up.right").frame(width: 44, height: 44)
        }
        .accessibilityLabel(Text("picture_forward"))
      }
    }
    .foregroundStyle(.white)
    .padding(.horizontal, 12)
    .padding(.vertical, 4)
    .background(.ultraThinMaterial)
  }

  private var scopeBinding: Binding<MediaScope> {
    Binding(
      get: { model.scope },
      set: { scope in Task { await model.setScope(scope, app: app) } })
  }

  private var shownFile: FileInfo? { model.currentFile ?? model.target.file }

  private var senderName: String? {
    shownFile?.maybeSender.flatMap { displayNames[$0] } ?? shownFile?.importedAuthor?.standInName
  }

  private var sentDate: Date? {
    shownFile?.maybeSentAt.map { Date(timeIntervalSince1970: TimeInterval($0) / 1000) }
  }

  /// The downloaded original, to share (save to Photos, send elsewhere): the user's explicit choice.
  private var localURL: URL? {
    guard let file = model.currentFile, file.transfer.state == "local", let path = file.transfer.path else {
      return nil
    }
    return URL(fileURLWithPath: path)
  }
}

/// A message to forward from the viewer.
private struct ForwardTarget: Identifiable {
  let conversationId: String
  let messageId: String

  var id: String { messageId }
}

/// One page of the viewer: the original when it is here, else the thumbnail (softened when small),
/// with the original's progress, "Load original" or "Try again" over it.
private struct PicturePage: View {
  let maybeFile: FileInfo?
  let maybeOriginal: UIImage?
  let state: OriginalState
  let maxZoom: CGFloat
  let doubleTapZoom: CGFloat
  let onTap: () -> Void
  let onZoomChange: (Bool) -> Void
  let onFetch: () -> Void

  /// Thumbnails smaller than this are blurred while they stand in for the original.
  private static let sharpThumbnailPixels: CGFloat = 480

  var body: some View {
    ZStack(alignment: .bottom) {
      ZoomableImage(
        image: maybeOriginal ?? thumbnail, maxZoom: maxZoom, doubleTapZoom: doubleTapZoom, onTap: onTap,
        onZoomChange: onZoomChange
      )
      .blur(radius: isSoftened ? 12 : 0)
      .accessibilityLabel(Text(maybeFile?.fileName ?? ""))
      .accessibilityAddTraits(.isImage)
      overlay.padding(.bottom, 96)
    }
  }

  private var thumbnail: UIImage? { maybeFile.flatMap(ThumbnailCache.image(for:)) }

  private var isSoftened: Bool {
    guard maybeOriginal == nil, let image = thumbnail else { return false }
    return max(image.size.width, image.size.height) * image.scale < Self.sharpThumbnailPixels
  }

  @ViewBuilder
  private var overlay: some View {
    switch state {
    case .loading(let maybeProgress):
      Group {
        if let progress = maybeProgress {
          ProgressRing(progress: progress)
        } else {
          ProgressView().tint(.white)
        }
      }
      .frame(width: 28, height: 28)
      .padding(10)
      .background(.ultraThinMaterial, in: Circle())
      .accessibilityElement()
      .accessibilityLabel(Text("picture_loading"))
      .accessibilityValue(maybeProgress.map { Text($0, format: .percent.precision(.fractionLength(0))) } ?? Text(verbatim: ""))
    case .onRequest:
      Button(action: onFetch) {
        Label(loadLabel, systemImage: "arrow.down.circle")
          .font(.subheadline.weight(.semibold))
          .padding(.horizontal, 14).padding(.vertical, 8)
          .background(.ultraThinMaterial, in: Capsule())
      }
      .foregroundStyle(.white)
    case .failed:
      VStack(spacing: 8) {
        Text("picture_load_failed").font(.subheadline).multilineTextAlignment(.center)
        Button("picture_try_again", action: onFetch)
          .font(.subheadline.weight(.semibold))
          .buttonStyle(.borderedProminent)
      }
      .foregroundStyle(.white)
      .padding(14)
      .background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: 14, style: .continuous))
    case .unreadable:
      Text("picture_unreadable").font(.subheadline)
        .foregroundStyle(.white)
        .padding(.horizontal, 14).padding(.vertical, 8)
        .background(.ultraThinMaterial, in: Capsule())
    case .shown, .thumbnailOnly:
      EmptyView()
    }
  }

  private var loadLabel: String {
    guard let file = maybeFile else { return String(localized: "picture_load_original") }
    return String(localized: "picture_load_original_size \(FileSizeText.format(file.sizeBytes))")
  }
}

/// How far a download got, as a ring that fills clockwise from the top.
private struct ProgressRing: View {
  let progress: Double

  var body: some View {
    ZStack {
      Circle().stroke(Color.white.opacity(0.3), lineWidth: 3)
      Circle()
        .trim(from: 0, to: min(max(progress, 0), 1))
        .stroke(Color.white, style: StrokeStyle(lineWidth: 3, lineCap: .round))
        .rotationEffect(.degrees(-90))
        .animation(.linear(duration: 0.2), value: progress)
    }
    .padding(2)
  }
}

/// A picture that zooms and pans like Photos: pinch and double tap within the shared limits, soft
/// edges (it rubber-bands past them), centred while it is smaller than the screen. UIKit's scroll
/// view, because it already zooms around the fingers with momentum and bounce.
private struct ZoomableImage: UIViewRepresentable {
  let image: UIImage?
  let maxZoom: CGFloat
  let doubleTapZoom: CGFloat
  let onTap: () -> Void
  let onZoomChange: (Bool) -> Void

  func makeCoordinator() -> Coordinator { Coordinator(self) }

  func makeUIView(context: Context) -> ZoomingScrollView {
    let scroll = ZoomingScrollView()
    scroll.delegate = context.coordinator
    scroll.minimumZoomScale = 1
    scroll.maximumZoomScale = maxZoom
    scroll.bouncesZoom = true
    scroll.showsHorizontalScrollIndicator = false
    scroll.showsVerticalScrollIndicator = false
    scroll.contentInsetAdjustmentBehavior = .never
    scroll.decelerationRate = .fast
    let doubleTap = UITapGestureRecognizer(target: context.coordinator, action: #selector(Coordinator.doubleTapped(_:)))
    doubleTap.numberOfTapsRequired = 2
    scroll.addGestureRecognizer(doubleTap)
    let tap = UITapGestureRecognizer(target: context.coordinator, action: #selector(Coordinator.tapped))
    tap.require(toFail: doubleTap)
    scroll.addGestureRecognizer(tap)
    scroll.show(image)
    return scroll
  }

  func updateUIView(_ scroll: ZoomingScrollView, context: Context) {
    context.coordinator.parent = self
    scroll.maximumZoomScale = maxZoom
    if scroll.imageView.image !== image { scroll.show(image) }
  }

  final class Coordinator: NSObject, UIScrollViewDelegate {
    var parent: ZoomableImage
    private var wasZoomed = false

    init(_ parent: ZoomableImage) {
      self.parent = parent
    }

    func viewForZooming(in scrollView: UIScrollView) -> UIView? {
      (scrollView as? ZoomingScrollView)?.imageView
    }

    func scrollViewDidZoom(_ scrollView: UIScrollView) {
      (scrollView as? ZoomingScrollView)?.centerImage()
      let isZoomed = scrollView.zoomScale > 1.01
      if isZoomed != wasZoomed {
        wasZoomed = isZoomed
        parent.onZoomChange(isZoomed)
      }
    }

    @objc func tapped() {
      parent.onTap()
    }

    /// A fitted picture zooms in around the tap; a zoomed one goes back to fit.
    @objc func doubleTapped(_ recognizer: UITapGestureRecognizer) {
      guard let scroll = recognizer.view as? ZoomingScrollView else { return }
      if scroll.zoomScale > 1.01 {
        scroll.setZoomScale(1, animated: true)
        return
      }
      let point = recognizer.location(in: scroll.imageView)
      let zoom = min(parent.doubleTapZoom, scroll.maximumZoomScale)
      let size = CGSize(width: scroll.bounds.width / zoom, height: scroll.bounds.height / zoom)
      scroll.zoom(
        to: CGRect(x: point.x - size.width / 2, y: point.y - size.height / 2, width: size.width, height: size.height),
        animated: true)
    }
  }
}

/// The scroll view of a page: its picture fitted into the bounds and centred.
private final class ZoomingScrollView: UIScrollView {
  let imageView = UIImageView()
  private var fittedBounds = CGSize.zero

  override init(frame: CGRect) {
    super.init(frame: frame)
    imageView.contentMode = .scaleAspectFit
    addSubview(imageView)
  }

  @available(*, unavailable)
  required init?(coder: NSCoder) {
    fatalError("not used from storyboards")
  }

  /// Shows `image`. The original replacing its thumbnail has the same shape: the zoom stays.
  func show(_ image: UIImage?) {
    let isSameShape = Self.ratio(of: imageView.image).map { old in
      Self.ratio(of: image).map { abs($0 - old) < 0.01 } ?? false
    } ?? false
    imageView.image = image
    if !isSameShape { fit() }
  }

  override func layoutSubviews() {
    super.layoutSubviews()
    if bounds.size != fittedBounds { fit() }
    centerImage()
  }

  func centerImage() {
    let horizontal = max(0, (bounds.width - contentSize.width) / 2)
    let vertical = max(0, (bounds.height - contentSize.height) / 2)
    contentInset = UIEdgeInsets(top: vertical, left: horizontal, bottom: vertical, right: horizontal)
  }

  private func fit() {
    fittedBounds = bounds.size
    setZoomScale(1, animated: false)
    guard let image = imageView.image, image.size.width > 0, image.size.height > 0, bounds.width > 0 else {
      imageView.frame = CGRect(origin: .zero, size: bounds.size)
      contentSize = bounds.size
      return
    }
    let scale = min(bounds.width / image.size.width, bounds.height / image.size.height)
    let size = CGSize(width: image.size.width * scale, height: image.size.height * scale)
    imageView.frame = CGRect(origin: .zero, size: size)
    contentSize = size
    centerImage()
  }

  private static func ratio(of image: UIImage?) -> CGFloat? {
    guard let image, image.size.height > 0 else { return nil }
    return image.size.width / image.size.height
  }
}

/// Opens the image viewer on a picture; set by the screens that show messages.
struct OpenPictureAction {
  let handler: (FileInfo) -> Void

  func callAsFunction(_ file: FileInfo) {
    handler(file)
  }
}

private struct OpenPictureKey: EnvironmentKey {
  static let defaultValue: OpenPictureAction? = nil
}

extension EnvironmentValues {
  var openPicture: OpenPictureAction? {
    get { self[OpenPictureKey.self] }
    set { self[OpenPictureKey.self] = newValue }
  }
}

extension FileInfo {
  /// A picture the viewer opens: one with a thumbnail to show first.
  var isViewablePicture: Bool { isImage && maybeThumbnailPng != nil }
}

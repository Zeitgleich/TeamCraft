import SwiftUI
import UIKit

/// The side a sticker in a message shows at, at most: large, but not a picture message.
let stickerMessageSide: CGFloat = 160

/// Workspace stickers (issue #2132) on the phone: the core verifies the packs' signatures and keeps
/// the pictures; this decodes them, shows them in messages and offers them in the sticker sheet.
/// Where this device doesn't hold a picture the message's fallback text shows, as in older apps.
@MainActor
enum StickerPictures {
  /// How often the app asks the core to read the packs again; the core paces its own reads too.
  private static let wantInterval: TimeInterval = 30
  /// Decoded pictures kept at once: a sheet of packs and the stickers on screen.
  private static let pictureLimit = 200

  private static let decoded: NSCache<NSString, UIImage> = {
    let cache = NSCache<NSString, UIImage>()
    cache.countLimit = pictureLimit
    return cache
  }()
  private static var lastWanted = Date.distantPast

  static func cached(_ sha256: String) -> UIImage? { decoded.object(forKey: sha256 as NSString) }

  /// The picture, decoded once; nil until the core holds it (it downloads after being asked).
  static func picture(_ sha256: String, core: TeamCraft) async -> UIImage? {
    if let image = cached(sha256) { return image }
    guard let bytes = try? await core.stickerPicture(sha256: sha256), let image = UIImage(data: bytes) else {
      return nil
    }
    decoded.setObject(image, forKey: sha256 as NSString)
    return image
  }

  /// Asks the core to read the packs unless that was asked lately. Returns when its read is done.
  static func want(core: TeamCraft) async {
    let now = Date()
    guard now.timeIntervalSince(lastWanted) >= wantInterval else { return }
    lastWanted = now
    // The core retries when asked again; a picture is not worth an error.
    try? await core.wantStickerPacks()
  }

  /// The packs the core holds verified; none while it can't say.
  static func packs(core: TeamCraft) async -> [StickerPackItem] {
    (try? await core.stickerPacks()) ?? []
  }
}

/// One sticker's picture, at most `side` points on its longer side, or `fallback` while this device
/// lacks it. Looks again when the store changes, as the picture arrives after the core asked.
struct StickerPictureView<Fallback: View>: View {
  @Environment(AppModel.self) private var model
  let sha256: String
  let alt: String
  let side: CGFloat
  let fallback: () -> Fallback
  @State private var maybePicture: UIImage?

  init(
    sha256: String, alt: String, side: CGFloat = stickerMessageSide,
    @ViewBuilder fallback: @escaping () -> Fallback
  ) {
    self.sha256 = sha256
    self.alt = alt
    self.side = side
    self.fallback = fallback
  }

  var body: some View {
    Group {
      if let image = maybePicture ?? StickerPictures.cached(sha256) {
        Image(uiImage: image)
          .resizable()
          .aspectRatio(contentMode: .fit)
          .frame(maxWidth: side, maxHeight: side)
          .accessibilityLabel(Text(verbatim: alt))
      } else {
        fallback()
      }
    }
    .task(id: maybePicture == nil ? model.changeCount : -1) {
      guard maybePicture == nil, let core = try? await model.core() else { return }
      maybePicture = await StickerPictures.picture(sha256, core: core)
      if maybePicture == nil { await StickerPictures.want(core: core) }
    }
  }
}

/// A sticker message: the picture large and without a bubble, or the message's fallback text
/// ("[Sticker: …]") while this device lacks the picture.
struct StickerMessageView: View {
  let sticker: StickerRef
  /// The message's text: the fallback the sender's app wrote.
  let fallbackText: String

  var body: some View {
    StickerPictureView(sha256: sticker.sha256, alt: sticker.alt) {
      Text(verbatim: fallbackText)
        .font(.app(.body))
        .foregroundStyle(Theme.text)
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(Theme.otherBubble, in: RoundedRectangle(cornerRadius: Theme.radius, style: .continuous))
    }
  }
}

/// The sticker sheet (issue #2132): the workspace's packs as capsules on top, the chosen pack's
/// stickers in a grid below. A tap sends the sticker; the sheet closes once the core took it and
/// stays open with the reason when it refused.
struct StickerSheet: View {
  @Environment(AppModel.self) private var model
  @Environment(\.dismiss) private var dismiss
  let conversationId: String
  let maybeThreadId: String?
  /// After the core took the sticker: the composer's send feedback.
  let onSent: () -> Void
  @State private var packs: [StickerPackItem] = []
  @State private var hasRead = false
  @State private var maybeChosenPack: String?
  @State private var isSending = false
  @State private var maybeFailure: AppFailure?

  private static let columns = Array(repeating: GridItem(.flexible(), spacing: 12), count: 4)
  private static let cellSide: CGFloat = 72

  var body: some View {
    NavigationStack {
      content
        .navigationTitle("sticker_sheet_title")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
          ToolbarItem(placement: .cancellationAction) {
            Button("cancel") { dismiss() }.disabled(isSending)
          }
        }
    }
    .interactiveDismissDisabled(isSending)
    // What the core holds now, and again on each change: a pack or a picture can arrive later.
    .task(id: model.changeCount) {
      guard let core = try? await model.core() else { return }
      packs = await StickerPictures.packs(core: core)
      hasRead = true
    }
    .task {
      guard let core = try? await model.core() else { return }
      await StickerPictures.want(core: core)
    }
  }

  @ViewBuilder
  private var content: some View {
    if let pack = chosenPack {
      VStack(spacing: 0) {
        if let maybeFailure {
          Label { Text(maybeFailure.message) } icon: { Image(systemName: "exclamationmark.triangle.fill") }
            .font(.app(.subheadline))
            .foregroundStyle(Theme.danger)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal)
            .padding(.top, 8)
        }
        if packs.count > 1 { packPicker }
        if pack.stickers.isEmpty {
          Text("sticker_pack_empty")
            .font(.app(.body))
            .foregroundStyle(Theme.textMuted)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .padding()
        } else {
          stickerGrid(pack)
        }
      }
    } else if hasRead {
      ContentUnavailableView("sticker_none", systemImage: "rectangle.stack")
    } else {
      ProgressView()
    }
  }

  private func stickerGrid(_ pack: StickerPackItem) -> some View {
    ScrollView {
      LazyVGrid(columns: Self.columns, spacing: 12) {
        ForEach(pack.stickers, id: \.name) { sticker in
          stickerButton(sticker, pack: pack.pack)
        }
      }
      .padding()
    }
  }

  /// The pack the grid shows: the one chosen, else the first.
  private var chosenPack: StickerPackItem? {
    packs.first { $0.pack == maybeChosenPack } ?? packs.first
  }

  private var packPicker: some View {
    ScrollView(.horizontal, showsIndicators: false) {
      HStack(spacing: 8) {
        ForEach(packs, id: \.pack) { pack in
          let isChosen = pack.pack == chosenPack?.pack
          Button {
            maybeChosenPack = pack.pack
          } label: {
            Text(verbatim: pack.name)
              .font(.app(.subheadline).weight(isChosen ? .semibold : .regular))
              .lineLimit(1)
              .foregroundStyle(isChosen ? Theme.onAccent : Theme.text)
              .padding(.horizontal, 14)
              .padding(.vertical, 8)
              .background(isChosen ? Theme.accent : Theme.surface, in: Capsule())
              .overlay(Capsule().stroke(isChosen ? Color.clear : Theme.border))
              .contentShape(Capsule())
          }
          .buttonStyle(.plain)
          .accessibilityAddTraits(isChosen ? .isSelected : [])
        }
      }
      .padding(.horizontal)
      .padding(.vertical, 8)
    }
  }

  private func stickerButton(_ sticker: StickerItem, pack: String) -> some View {
    Button {
      send(sticker, pack: pack)
    } label: {
      StickerPictureView(sha256: sticker.sha256, alt: sticker.alt, side: Self.cellSide) {
        // Not downloaded yet: the room it takes, so the grid doesn't jump when it arrives.
        RoundedRectangle(cornerRadius: Theme.radius, style: .continuous)
          .fill(Theme.surface)
      }
      .frame(width: Self.cellSide, height: Self.cellSide)
      .contentShape(Rectangle())
    }
    .buttonStyle(.plain)
    .disabled(isSending)
    .accessibilityElement(children: .ignore)
    .accessibilityLabel(Text(verbatim: sticker.alt))
    .accessibilityAddTraits(.isButton)
  }

  private func send(_ sticker: StickerItem, pack: String) {
    guard !isSending else { return }
    isSending = true
    maybeFailure = nil
    // What older apps and push previews show instead of the picture.
    let fallback = String(localized: "sticker_fallback \(sticker.alt)")
    Task {
      do {
        try await model.core().sendSticker(
          conversationId: conversationId, maybeThreadId: maybeThreadId, pack: pack, name: sticker.name,
          fallback: fallback)
        isSending = false
        onSent()
        dismiss()
      } catch {
        // The sheet stays open with the reason; the user can try again.
        maybeFailure = AppFailure(CoreFailure(error))
        isSending = false
      }
    }
  }
}

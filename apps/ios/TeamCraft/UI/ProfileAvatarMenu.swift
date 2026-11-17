import PhotosUI
import SwiftUI
import UIKit

/// The member's picture at the top of Settings. Tapping it opens a menu with the rare profile
/// actions (issue #2071): change the picture (photos, then move and zoom under a circle), remove it,
/// change the name or go back to the sign-in name (#408). Nothing takes a row until it is used.
/// While the workspace shows the identity provider's picture without overrides, the menu says where
/// the picture comes from instead. Errors show below the header (`maybeProblem`), or in the crop
/// screen while it is open; the screen closes once the server took the picture.
struct ProfileAvatarMenu: View {
  @Environment(AppModel.self) private var model
  let account: Account
  let maybeProfileJSON: String?
  /// The picture shown; set here after a change.
  @Binding var maybePicture: UIImage?
  @Binding var maybeProblem: TextProblem?
  let onEditName: () -> Void
  let onResetName: () -> Void
  /// Reads the profile again after a change.
  let onChanged: () async -> Void

  @State private var selection: PhotosPickerItem?
  @State private var maybePicked: UIImage?
  @State private var isSaving = false

  var body: some View {
    Menu {
      if let profile = maybeProfileJSON {
        if OwnProfileDocument.canChangeAvatar(in: profile) {
          PhotosPicker(selection: $selection, matching: .images, photoLibrary: .shared()) {
            Label("profile_picture_change", systemImage: "photo")
          }
          if OwnProfileDocument.hasAvatar(in: profile) {
            Button("profile_picture_remove", systemImage: "trash", role: .destructive) { Task { await remove() } }
          }
        } else {
          Text("profile_picture_from_provider")
        }
        if OwnProfileDocument.canOverrideDisplayName(in: profile) {
          Button("profile_name_change", systemImage: "pencil", action: onEditName)
          if OwnProfileDocument.displayNameOverride(in: profile) != nil {
            Button("profile_name_reset", systemImage: "arrow.uturn.backward", action: onResetName)
          }
        }
      }
    } label: {
      avatar
        .overlay(alignment: .bottomTrailing) {
          Image(systemName: "pencil.circle.fill")
            .symbolRenderingMode(.palette)
            .foregroundStyle(Theme.textMuted, Color(uiColor: .secondarySystemBackground))
            .font(.system(size: 16))
            .offset(x: 2, y: 2)
            .accessibilityHidden(true)
        }
    }
    .disabled(maybeProfileJSON == nil || isSaving)
    .accessibilityLabel("profile_actions")
    .onChange(of: selection) { Task { await load(selection) } }
    .fullScreenCover(isPresented: Binding(get: { maybePicked != nil }, set: { if !$0 { maybePicked = nil } })) {
      if let picked = maybePicked {
        AvatarCropScreen(
          picture: picked,
          isSaving: isSaving,
          maybeProblem: maybeProblem,
          onCancel: {
            maybePicked = nil
            maybeProblem = nil
          },
          onUse: { square in Task { await upload(picked, square: square) } }
        )
      }
    }
  }

  @ViewBuilder private var avatar: some View {
    if let picture = maybePicture {
      Image(uiImage: picture)
        .resizable()
        .scaledToFill()
        .frame(width: 44, height: 44)
        .clipShape(Circle())
    } else {
      AvatarView(name: account.displayName, size: 44)
    }
  }

  private func load(_ maybeItem: PhotosPickerItem?) async {
    guard let item = maybeItem else { return }
    selection = nil
    maybeProblem = nil
    guard let data = try? await item.loadTransferable(type: Data.self),
      let image = UIImage(data: data),
      let upright = image.uprightScaled(maxSide: Self.maxEditSide)
    else {
      maybeProblem = .rejected("profile_picture_unreadable")
      return
    }
    maybePicked = upright
  }

  private func upload(_ picture: UIImage, square: CGRect) async {
    guard !isSaving else { return }
    isSaving = true
    maybeProblem = nil
    guard let png = picture.croppedPNG(square: square, maxSide: Self.uploadSide) else {
      maybeProblem = .rejected("profile_picture_unreadable")
      isSaving = false
      return
    }
    if let problem = await model.setOwnAvatar(png: png) {
      maybeProblem = problem
    } else {
      maybePicture = UIImage(data: png)
      maybePicked = nil
      await onChanged()
    }
    isSaving = false
  }

  private func remove() async {
    guard !isSaving else { return }
    isSaving = true
    maybeProblem = await model.removeOwnAvatar()
    if maybeProblem == nil {
      maybePicture = nil
      await onChanged()
    }
    isSaving = false
  }

  /// The picture is drawn at most this large (pixels) for the crop screen.
  private static let maxEditSide: CGFloat = 2048
  /// The side of the square sent to the server; it re-encodes and limits it again.
  private static let uploadSide: CGFloat = 512
}

/// Full screen: the picture moves (drag) and zooms (pinch, or the slider for those who can't) under
/// a fixed circle; outside the circle is dimmed. Cancel and Use in the toolbar, like Photos.
private struct AvatarCropScreen: View {
  let picture: UIImage
  let isSaving: Bool
  let maybeProblem: TextProblem?
  let onCancel: () -> Void
  let onUse: (CGRect) -> Void

  @State private var crop: AvatarCrop
  @State private var dragStart: AvatarCrop?
  @State private var zoomStart: AvatarCrop?

  init(
    picture: UIImage, isSaving: Bool, maybeProblem: TextProblem?, onCancel: @escaping () -> Void,
    onUse: @escaping (CGRect) -> Void
  ) {
    self.picture = picture
    self.isSaving = isSaving
    self.maybeProblem = maybeProblem
    self.onCancel = onCancel
    self.onUse = onUse
    _crop = State(initialValue: AvatarCrop(pictureWidth: picture.size.width, pictureHeight: picture.size.height))
  }

  var body: some View {
    NavigationStack {
      VStack(spacing: 16) {
        GeometryReader { geometry in
          let circleSide = max(min(geometry.size.width, geometry.size.height) - 48, 1)
          let scale = circleSide / crop.side
          let middle = CGPoint(x: geometry.size.width / 2, y: geometry.size.height / 2)
          let origin = CGPoint(x: middle.x - circleSide / 2 - crop.left * scale, y: middle.y - circleSide / 2 - crop.top * scale)
          let shownWidth = crop.pictureWidth * scale
          let shownHeight = crop.pictureHeight * scale
          ZStack {
            Image(uiImage: picture)
              .resizable()
              .interpolation(.medium)
              .frame(width: shownWidth, height: shownHeight)
              .position(x: origin.x + shownWidth / 2, y: origin.y + shownHeight / 2)
            Rectangle()
              .fill(Color.black.opacity(0.6))
              .mask {
                Rectangle()
                  .overlay { Circle().frame(width: circleSide, height: circleSide).blendMode(.destinationOut) }
                  .compositingGroup()
              }
              .allowsHitTesting(false)
            Circle()
              .stroke(Color.white.opacity(0.8), lineWidth: 1)
              .frame(width: circleSide, height: circleSide)
              .allowsHitTesting(false)
          }
          .frame(width: geometry.size.width, height: geometry.size.height)
          .contentShape(Rectangle())
          .clipped()
          .gesture(
            SimultaneousGesture(
              DragGesture(minimumDistance: 0)
                .onChanged { value in
                  let start = dragStart ?? crop
                  dragStart = start
                  crop = start.zoomed(to: crop.zoom).dragged(
                    dx: value.translation.width, dy: value.translation.height, viewSide: circleSide)
                }
                .onEnded { _ in dragStart = nil },
              MagnifyGesture()
                .onChanged { value in
                  let start = zoomStart ?? crop
                  zoomStart = start
                  crop = crop.zoomed(to: start.zoom * value.magnification)
                }
                .onEnded { _ in zoomStart = nil }
            )
          )
          .accessibilityElement()
          .accessibilityLabel("profile_picture_crop_area")
        }
        if crop.maxZoom > 1.01 {
          Slider(value: Binding(get: { crop.zoom }, set: { crop = crop.zoomed(to: $0) }), in: 1...crop.maxZoom) {
            Text("profile_picture_zoom")
          }
          .disabled(isSaving)
          .padding(.horizontal)
        }
        FieldMessage(maybeError: maybeProblem?.message)
          .padding(.horizontal)
      }
      .padding(.vertical)
      .background(Color.black)
      .navigationTitle("profile_picture_crop_title")
      .navigationBarTitleDisplayMode(.inline)
      .toolbarColorScheme(.dark, for: .navigationBar)
      .toolbar {
        ToolbarItem(placement: .cancellationAction) {
          Button("cancel", action: onCancel).disabled(isSaving)
        }
        ToolbarItem(placement: .confirmationAction) {
          if isSaving {
            ProgressView()
          } else {
            Button("profile_picture_use") { onUse(crop.square) }
          }
        }
      }
    }
  }
}

extension UIImage {
  /// The picture drawn upright (EXIF orientation applied) at most `maxSide` pixels per side, at
  /// scale 1 so its size is in pixels.
  func uprightScaled(maxSide: CGFloat) -> UIImage? {
    let pixelWidth = size.width * scale
    let pixelHeight = size.height * scale
    guard pixelWidth > 0, pixelHeight > 0 else { return nil }
    let factor = min(1, maxSide / max(pixelWidth, pixelHeight))
    let target = CGSize(width: (pixelWidth * factor).rounded(), height: (pixelHeight * factor).rounded())
    let format = UIGraphicsImageRendererFormat()
    format.scale = 1
    return UIGraphicsImageRenderer(size: target, format: format).image { _ in
      draw(in: CGRect(origin: .zero, size: target))
    }
  }

  /// The `square` (in this upright picture's pixels) as a PNG at most `maxSide` pixels wide.
  func croppedPNG(square: CGRect, maxSide: CGFloat) -> Data? {
    guard square.width > 0 else { return nil }
    let side = min(square.width, maxSide)
    let factor = side / square.width
    let format = UIGraphicsImageRendererFormat()
    format.scale = 1
    format.opaque = false
    let renderer = UIGraphicsImageRenderer(size: CGSize(width: side, height: side), format: format)
    return renderer.pngData { _ in
      draw(in: CGRect(
        x: -square.minX * factor, y: -square.minY * factor,
        width: size.width * factor, height: size.height * factor))
    }
  }
}

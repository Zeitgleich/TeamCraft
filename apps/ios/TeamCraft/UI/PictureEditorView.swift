import SwiftUI

/// Edits a picture before it is sent, as on Android: turn it a quarter at a time and crop it by
/// dragging the frame's corners or the frame itself. Done hands back the edited picture; Cancel
/// leaves the original. Dark, whatever the theme, like a photo viewer.
struct PictureEditorView: View {
  let file: PickedFile
  let onDone: (PickedFile) -> Void
  @Environment(\.dismiss) private var dismiss
  @State private var original: UIImage?
  @State private var shown: UIImage?
  @State private var turns = 0
  @State private var frame = CropFrame()
  @State private var isSaving = false
  @State private var isFailed = false

  var body: some View {
    VStack(spacing: 0) {
      ZStack {
        if isFailed {
          Text("picture_edit_failed").foregroundStyle(.white).padding()
        } else if let shown {
          CropCanvas(image: shown, frame: $frame)
        } else {
          ProgressView().tint(.white)
        }
      }
      .frame(maxWidth: .infinity, maxHeight: .infinity)
      HStack {
        Button("cancel") { dismiss() }.foregroundStyle(.white)
        Spacer()
        Button {
          turns = (turns + 1) % 4
          frame = frame.turnedLeft()
        } label: {
          Image(systemName: "rotate.left").foregroundStyle(.white)
        }
        .accessibilityLabel(Text("picture_rotate"))
        .disabled(shown == nil || isSaving)
        Button("picture_reset") {
          turns = 0
          frame = CropFrame()
        }
        .foregroundStyle(.white)
        .disabled(turns == 0 && frame.isWhole)
        Spacer()
        Button("picture_edit_done") { save() }
          .buttonStyle(.borderedProminent)
          .disabled(shown == nil || isSaving)
      }
      .padding(.horizontal, 16)
      .padding(.vertical, 10)
    }
    .background(Color.black.ignoresSafeArea())
    .task {
      let path = file.path
      let loaded = await Task.detached { PictureEdits.upright(path: path, maxSide: 1600) }.value
      original = loaded
      shown = loaded
      isFailed = loaded == nil
    }
    .onChange(of: turns) { _, turns in
      if let original { shown = PictureEdits.turned(original, quarterTurnsLeft: turns) }
    }
  }

  private func save() {
    if turns == 0 && frame.isWhole {
      dismiss()
      return
    }
    isSaving = true
    let file = file
    let turns = turns
    let frame = frame
    Task {
      let maybeEdited = await Task.detached { PictureEdits.render(file, quarterTurnsLeft: turns, frame: frame) }.value
      isSaving = false
      if let edited = maybeEdited {
        onDone(edited)
        dismiss()
      } else {
        isFailed = true
      }
    }
  }
}

/// The picture fitted into the space, the area outside the frame dimmed, the frame with corners.
private struct CropCanvas: View {
  let image: UIImage
  @Binding var frame: CropFrame
  @State private var maybeDrag: Drag?

  private enum Drag {
    case corner(CropCorner)
    case frame
  }

  var body: some View {
    GeometryReader { geometry in
      let margin: CGFloat = 24
      let area = CGSize(width: geometry.size.width - margin * 2, height: geometry.size.height - margin * 2)
      let scale = min(area.width / image.size.width, area.height / image.size.height)
      let drawn = CGSize(width: image.size.width * scale, height: image.size.height * scale)
      let picture = CGRect(
        x: margin + (area.width - drawn.width) / 2, y: margin + (area.height - drawn.height) / 2,
        width: drawn.width, height: drawn.height)
      let crop = CGRect(
        x: picture.minX + frame.left * picture.width, y: picture.minY + frame.top * picture.height,
        width: frame.width * picture.width, height: frame.height * picture.height)
      ZStack(alignment: .topLeading) {
        Image(uiImage: image).resizable().frame(width: picture.width, height: picture.height)
          .position(x: picture.midX, y: picture.midY)
        Path { path in
          path.addRect(picture)
          path.addRect(crop)
        }
        .fill(Color.black.opacity(0.55), style: FillStyle(eoFill: true))
        Rectangle().stroke(.white, lineWidth: 1).frame(width: crop.width, height: crop.height)
          .position(x: crop.midX, y: crop.midY)
        ForEach(Array(corners(of: crop).enumerated()), id: \.offset) { _, point in
          Circle().fill(.white).frame(width: 14, height: 14).position(point)
        }
      }
      .contentShape(Rectangle())
      .gesture(
        DragGesture(minimumDistance: 0)
          .onChanged { value in
            if maybeDrag == nil { maybeDrag = drag(at: value.startLocation, crop: crop) }
            let last = value.translation
            defer { previous = last }
            let dx = (last.width - previous.width) / picture.width
            let dy = (last.height - previous.height) / picture.height
            switch maybeDrag {
            case .corner(let corner): frame = frame.dragged(corner, dx: dx, dy: dy)
            case .frame: frame = frame.moved(dx: dx, dy: dy)
            case nil: break
            }
          }
          .onEnded { _ in
            maybeDrag = nil
            previous = .zero
          }
      )
      .accessibilityElement()
      .accessibilityLabel(Text("picture_crop_area"))
    }
  }

  @State private var previous = CGSize.zero

  private func corners(of crop: CGRect) -> [CGPoint] {
    [
      CGPoint(x: crop.minX, y: crop.minY), CGPoint(x: crop.maxX, y: crop.minY),
      CGPoint(x: crop.minX, y: crop.maxY), CGPoint(x: crop.maxX, y: crop.maxY),
    ]
  }

  private func drag(at point: CGPoint, crop: CGRect) -> Drag? {
    let candidates: [(CropCorner, CGPoint)] = [
      (.topLeft, CGPoint(x: crop.minX, y: crop.minY)), (.topRight, CGPoint(x: crop.maxX, y: crop.minY)),
      (.bottomLeft, CGPoint(x: crop.minX, y: crop.maxY)), (.bottomRight, CGPoint(x: crop.maxX, y: crop.maxY)),
    ]
    let distance = { (a: CGPoint) in hypot(a.x - point.x, a.y - point.y) }
    if let nearest = candidates.min(by: { distance($0.1) < distance($1.1) }), distance(nearest.1) <= 40 {
      return .corner(nearest.0)
    }
    return crop.contains(point) ? .frame : nil
  }
}

/// A picked picture's thumbnail in the composer, decoded once per file (not on every keystroke).
struct PickedPictureThumbnail: View {
  let path: String
  @State private var maybeImage: UIImage?

  var body: some View {
    Group {
      if let image = maybeImage {
        Image(uiImage: image).resizable().scaledToFill()
      } else {
        Theme.accentSoft
      }
    }
    .task(id: path) {
      let path = path
      maybeImage = await Task.detached { UIImage(contentsOfFile: path)?.preparingThumbnail(of: CGSize(width: 168, height: 168)) }.value
    }
  }
}

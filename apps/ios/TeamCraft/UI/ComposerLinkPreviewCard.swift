import SwiftUI
import UIKit

/// The link preview under the composer's draft (#2073): what the message will carry, made by the
/// core while the user types. While the page loads it names the site; the close button takes the
/// preview away, and the message then goes without one. Nothing shows without a link or a preview.
struct ComposerLinkPreviewCard: View {
  let preview: ComposerLinkPreview
  let onRemove: () -> Void

  private static let thumbSide: CGFloat = 44

  var body: some View {
    HStack(spacing: 10) {
      thumbnail
      VStack(alignment: .leading, spacing: 1) {
        if let ready = preview.maybeReady {
          Text(verbatim: ready.maybeSiteName ?? Self.host(of: ready.url))
            .font(.app(.caption)).foregroundStyle(Theme.textMuted).lineLimit(1)
          Text(verbatim: ready.maybeTitle ?? ready.url)
            .font(.app(.subheadline).weight(.semibold)).lineLimit(1)
          if let description = ready.maybeDescription {
            Text(verbatim: description).font(.app(.caption)).foregroundStyle(Theme.textMuted).lineLimit(1)
          }
        } else {
          Text(verbatim: Self.host(of: preview.url ?? ""))
            .font(.app(.caption)).foregroundStyle(Theme.textMuted).lineLimit(1)
          Text("composer_link_preview_loading")
            .font(.app(.caption)).foregroundStyle(Theme.textMuted).lineLimit(1)
        }
      }
      .frame(maxWidth: .infinity, alignment: .leading)
      Button(action: onRemove) {
        Image(systemName: "xmark.circle.fill")
          .font(.title3)
          .frame(width: 44, height: 44)
          .contentShape(Rectangle())
      }
      .foregroundStyle(Theme.textMuted)
      .accessibilityLabel(Text("composer_link_preview_remove"))
    }
    .padding(.leading, 11)
    .padding(.vertical, 6)
    .background(Theme.surface)
    .overlay(alignment: .leading) {
      Rectangle().fill(preview.maybeReady != nil ? Theme.accent : Theme.border).frame(width: 3)
    }
    .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
    .overlay(RoundedRectangle(cornerRadius: 10, style: .continuous).stroke(Theme.border, lineWidth: 1))
    .accessibilityElement(children: .contain)
  }

  @ViewBuilder
  private var thumbnail: some View {
    if let data = preview.maybeReady?.maybeImageData, let image = UIImage(data: data) {
      Image(uiImage: image).resizable().scaledToFill()
        .frame(width: Self.thumbSide, height: Self.thumbSide)
        .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
        .accessibilityHidden(true)
    } else {
      Image(systemName: "link")
        .font(.body)
        .foregroundStyle(Theme.textMuted)
        .frame(width: Self.thumbSide, height: Self.thumbSide)
        .background(Theme.background, in: RoundedRectangle(cornerRadius: 8, style: .continuous))
        .accessibilityHidden(true)
    }
  }

  /// The host of a web link, to name the site (`https://docs.example.com/guide` → `docs.example.com`).
  static func host(of url: String) -> String {
    guard let host = URL(string: url)?.host(), !host.isEmpty else { return url }
    return host
  }
}

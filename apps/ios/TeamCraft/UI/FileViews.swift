import QuickLook
import SwiftUI

/// A file in a message: thumbnail or icon, name, size, progress while it moves. A tap opens a
/// downloaded file in Quick Look, or downloads it first (the core decrypts it into the app's
/// private storage).
struct AttachmentView: View {
  @Environment(AppModel.self) private var model
  let file: FileInfo

  @State private var maybePreviewURL: URL?

  var body: some View {
    Button(action: open) {
      HStack(spacing: 10) {
        FileThumbnail(file: file, side: 56)
        VStack(alignment: .leading, spacing: 2) {
          Text(file.fileName).font(.subheadline.weight(.semibold)).lineLimit(2)
          Text(FileSizeText.format(file.sizeBytes)).font(.caption).foregroundStyle(Theme.textMuted)
          FileStateLine(file: file)
        }
        Spacer(minLength: 0)
      }
      .padding(8)
      .background(Theme.surface)
      .overlay(RoundedRectangle(cornerRadius: Theme.radius).stroke(Theme.border))
      .clipShape(RoundedRectangle(cornerRadius: Theme.radius))
    }
    .buttonStyle(.plain)
    .accessibilityLabel(Text(file.fileName))
    .quickLookPreview($maybePreviewURL)
  }

  private func open() {
    switch file.transfer.state {
    case "local":
      if let path = file.transfer.path { maybePreviewURL = URL(fileURLWithPath: path) }
    case "remote", "failed":
      Task { await model.perform(.downloadFile(conversationId: file.conversation, messageId: file.message)) }
    default:
      break
    }
  }
}

/// Progress, failure or the hint to tap, under a file's name.
struct FileStateLine: View {
  let file: FileInfo

  var body: some View {
    switch file.transfer.state {
    case "uploading", "downloading":
      VStack(alignment: .leading, spacing: 2) {
        if let progress = file.transfer.maybeProgress {
          ProgressView(value: progress)
        } else {
          ProgressView()
        }
        if file.transfer.state == "uploading" {
          Text("file_uploading").font(.caption2).foregroundStyle(Theme.textMuted)
        } else {
          Text("file_downloading").font(.caption2).foregroundStyle(Theme.textMuted)
        }
      }
    case "failed":
      Text("file_failed").font(.caption2).foregroundStyle(Theme.danger)
    case "remote":
      Text("file_tap_to_download").font(.caption2).foregroundStyle(Theme.textMuted)
    default:
      EmptyView()
    }
  }
}

/// The sender's thumbnail, or an icon by file kind.
struct FileThumbnail: View {
  let file: FileInfo
  let side: CGFloat

  var body: some View {
    Group {
      if let data = file.maybeThumbnailData, let image = UIImage(data: data) {
        Image(uiImage: image).resizable().scaledToFill()
      } else {
        Image(systemName: icon).font(.title2).foregroundStyle(Theme.textMuted)
          .frame(maxWidth: .infinity, maxHeight: .infinity).background(Theme.accentSoft)
      }
    }
    .frame(width: side, height: side)
    .clipShape(RoundedRectangle(cornerRadius: 8))
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

/// A link preview card from the data the sender's core fetched.
struct LinkPreviewCard: View {
  let preview: LinkPreview

  var body: some View {
    let content = VStack(alignment: .leading, spacing: 4) {
      if let data = preview.maybeImageData, let image = UIImage(data: data) {
        Image(uiImage: image).resizable().scaledToFit().frame(maxHeight: 160)
          .clipShape(RoundedRectangle(cornerRadius: 8))
      }
      if let site = preview.maybeSiteName {
        Text(site).font(.caption).foregroundStyle(Theme.textMuted)
      }
      if let title = preview.maybeTitle {
        Text(title).font(.subheadline.weight(.semibold)).lineLimit(2)
      }
      if let description = preview.maybeDescription {
        Text(description).font(.caption).foregroundStyle(Theme.textMuted).lineLimit(3)
      }
    }
    .frame(maxWidth: .infinity, alignment: .leading)
    .padding(10)
    .background(Theme.surface)
    .overlay(alignment: .leading) { Rectangle().fill(Theme.accent).frame(width: 3) }
    .clipShape(RoundedRectangle(cornerRadius: 8))
    if let url = preview.maybeOpenableURL {
      Link(destination: url) { content }.buttonStyle(.plain)
    } else {
      content
    }
  }
}

/// Every file of a conversation as a gallery; tap opens or downloads.
struct FilesGalleryView: View {
  @Environment(AppModel.self) private var model
  let conversation: ConversationItem

  @State private var files: [FileInfo]?
  @State private var maybePreviewURL: URL?

  private let columns = [GridItem(.adaptive(minimum: 110), spacing: 8)]

  var body: some View {
    Group {
      if let files, files.isEmpty {
        Text("files_empty").foregroundStyle(Theme.textMuted)
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
    .task(id: model.changeCount) { files = await model.files(conversationId: conversation.id) }
    .quickLookPreview($maybePreviewURL)
  }

  private func tile(_ file: FileInfo) -> some View {
    Button {
      if file.transfer.state == "local", let path = file.transfer.path {
        maybePreviewURL = URL(fileURLWithPath: path)
      } else if file.transfer.state == "remote" || file.transfer.state == "failed" {
        Task { await model.perform(.downloadFile(conversationId: file.conversation, messageId: file.message)) }
      }
    } label: {
      VStack(alignment: .leading, spacing: 4) {
        FileThumbnail(file: file, side: 110)
        Text(file.fileName).font(.caption).lineLimit(1)
        Text(FileSizeText.format(file.sizeBytes)).font(.caption2).foregroundStyle(Theme.textMuted)
        FileStateLine(file: file)
      }
      .frame(width: 110, alignment: .leading)
    }
    .buttonStyle(.plain)
  }
}

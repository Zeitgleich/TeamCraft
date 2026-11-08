import UIKit

/// What the composer's message replies to: a message, or one picture of an album (#241). Chosen in
/// the feed, or read back from the core after the app was killed (#1115): the core keeps only the
/// IDs, on this device, so a target read back names its author and picture from the store.
struct ComposerReply: Equatable {
  let messageId: String
  /// The one picture of an album the reply answers.
  let maybeAttachmentId: String?
  /// As the feed shows it; a target read back has `maybeAuthorId` instead.
  let maybeAuthorName: String?
  let maybeAuthorId: String?
  let excerpt: String
  /// The picture chosen in the feed, for its cached thumbnail.
  let maybeFile: FileInfo?
  /// A target read back: the thumbnail of the quote.
  let maybeThumbnailData: Data?

  init(message: MessageItem) {
    messageId = message.id
    maybeAttachmentId = nil
    maybeAuthorName = message.shownAuthorName
    maybeAuthorId = nil
    excerpt = oneLine(message.body)
    maybeFile = nil
    maybeThumbnailData = nil
  }

  init(image file: FileInfo) {
    messageId = file.message
    maybeAttachmentId = file.fileId
    maybeAuthorName = nil
    maybeAuthorId = file.maybeSender
    excerpt = file.fileName
    maybeFile = file
    maybeThumbnailData = nil
  }

  /// `nil` when the quote doesn't name its message any more.
  init?(stored: ReplyPreview) {
    guard let messageId = stored.maybeMessage else { return nil }
    self.messageId = messageId
    maybeAttachmentId = stored.attachment?.fileId
    maybeAuthorName = nil
    maybeAuthorId = stored.author
    excerpt = stored.attachment?.fileName ?? oneLine(stored.excerpt)
    maybeFile = nil
    maybeThumbnailData = stored.attachment?.thumbnailData
  }

  func authorName(from displayNames: [String: String]) -> String {
    maybeAuthorName ?? maybeAuthorId.flatMap { displayNames[$0] } ?? ""
  }

  /// The picture the bar shows beside the excerpt, if the reply answers one.
  var maybeThumbnail: UIImage? {
    if let file = maybeFile { return ThumbnailCache.image(for: file) }
    return maybeThumbnailData.flatMap { UIImage(data: $0) }
  }
}

import SwiftUI
import UIKit
import UniformTypeIdentifiers
import UserNotifications

/// The share sheet's entry for TeamCraft (issue #208). It copies what was shared into the App Group
/// and leaves the rest to the app: encryption, the conversation picker and the upload all need the
/// core, which doesn't belong in an extension's memory limit, and big videos go through here as
/// files only. An extension can't open its app, so it says so and posts a notification that does.
final class ShareViewController: UIViewController {
  private let progress = ShareProgress()

  override func viewDidLoad() {
    super.viewDidLoad()
    let host = UIHostingController(
      rootView: ShareStatusView(progress: progress) { [weak self] in
        self?.extensionContext?.completeRequest(returningItems: nil)
      })
    addChild(host)
    host.view.frame = view.bounds
    host.view.autoresizingMask = [.flexibleWidth, .flexibleHeight]
    view.addSubview(host.view)
    host.didMove(toParent: self)
    let items = extensionContext?.inputItems.compactMap { $0 as? NSExtensionItem } ?? []
    Task { await stage(items) }
  }

  private func stage(_ items: [NSExtensionItem]) async {
    guard let inbox = ShareInbox.shared(), var share = try? inbox.begin() else {
      progress.state = .failed
      return
    }
    var texts: [String] = []
    for item in items {
      if let text = item.attributedContentText?.string, !text.isEmpty { texts.append(text) }
      for provider in item.attachments ?? [] {
        await ShareLoader.load(provider, into: &share, texts: &texts, inbox: inbox)
      }
    }
    share.text = ShareLoader.joined(texts)
    guard !share.isEmpty, (try? inbox.commit(share)) != nil else {
      inbox.delete(share.id)
      progress.state = .failed
      return
    }
    progress.state = .saved
    await postOpenNotification()
  }

  /// A tap opens the app, which finds the share in the inbox. Without notification permission the
  /// sheet's text is what's left, and it says the same.
  private func postOpenNotification() async {
    let content = UNMutableNotificationContent()
    content.title = String(localized: "share_saved_title")
    content.body = String(localized: "share_saved_notification")
    let request = UNNotificationRequest(identifier: "share-saved", content: content, trigger: nil)
    try? await UNUserNotificationCenter.current().add(request)
  }
}

/// What the sheet shows.
@MainActor
@Observable
final class ShareProgress {
  enum State {
    case copying, saved, failed
  }

  var state = State.copying
}

/// Reads the share sheet's items into the inbox: files by the file system, text and links as text.
enum ShareLoader {
  static func load(
    _ provider: NSItemProvider, into share: inout StagedShare, texts: inout [String], inbox: ShareInbox
  ) async {
    // Pictures and videos first: a photo may also offer a URL or text next to its file.
    if let type = fileType(of: provider, among: [.image, .movie]) {
      await loadFile(provider, type: type, into: &share, inbox: inbox)
    } else if provider.hasItemConformingToTypeIdentifier(UTType.url.identifier) {
      let maybeURL = await item(of: provider, type: .url) as? URL
      if let url = maybeURL, !url.isFileURL {
        texts.append(url.absoluteString)
      } else if let type = fileType(of: provider, among: [.data]) {
        await loadFile(provider, type: type, into: &share, inbox: inbox)
      } else {
        share.leftOut += 1
      }
    } else if provider.hasItemConformingToTypeIdentifier(UTType.plainText.identifier) {
      if let text = await item(of: provider, type: .plainText) as? String {
        texts.append(text)
      } else {
        share.leftOut += 1
      }
    } else if let type = fileType(of: provider, among: [.data]) {
      await loadFile(provider, type: type, into: &share, inbox: inbox)
    } else {
      share.leftOut += 1
    }
  }

  /// Shared texts without repeats (a page's text often names its address), in order.
  static func joined(_ texts: [String]) -> String {
    var parts: [String] = []
    for text in texts.map({ $0.trimmingCharacters(in: .whitespacesAndNewlines) }) where !text.isEmpty {
      if !parts.contains(where: { $0.contains(text) }) { parts.append(text) }
    }
    return parts.joined(separator: "\n")
  }

  private static func fileType(of provider: NSItemProvider, among kinds: [UTType]) -> String? {
    provider.registeredTypeIdentifiers.first { identifier in
      guard let type = UTType(identifier) else { return false }
      return kinds.contains { type.conforms(to: $0) }
    }
  }

  private static func item(of provider: NSItemProvider, type: UTType) async -> NSSecureCoding? {
    await withCheckedContinuation { continuation in
      provider.loadItem(forTypeIdentifier: type.identifier, options: nil) { item, _ in
        continuation.resume(returning: item)
      }
    }
  }

  /// The system hands out a temporary file for the time of the callback; it is copied right there.
  private static func loadFile(
    _ provider: NSItemProvider, type: String, into share: inout StagedShare, inbox: ShareInbox
  ) async {
    let current = share
    let updated: StagedShare? = await withCheckedContinuation { continuation in
      _ = provider.loadFileRepresentation(forTypeIdentifier: type) { url, _ in
        guard let url else {
          continuation.resume(returning: nil)
          return
        }
        var copy = current
        do {
          try inbox.add(file: url, named: url.lastPathComponent, to: &copy)
          continuation.resume(returning: copy)
        } catch {
          continuation.resume(returning: nil)
        }
      }
    }
    if let updated {
      share = updated
    } else {
      share.leftOut += 1
    }
  }
}

/// A plain card: copying, then what happens next.
struct ShareStatusView: View {
  let progress: ShareProgress
  let onDone: () -> Void

  var body: some View {
    NavigationStack {
      VStack(spacing: 16) {
        switch progress.state {
        case .copying:
          ProgressView("share_copying")
        case .saved:
          Image(systemName: "checkmark.circle")
            .font(.largeTitle)
            .foregroundStyle(.tint)
            .accessibilityHidden(true)
          Text("share_saved_title").font(.headline)
          Text("share_saved_body").multilineTextAlignment(.center)
        case .failed:
          Image(systemName: "exclamationmark.triangle")
            .font(.largeTitle)
            .foregroundStyle(.red)
            .accessibilityHidden(true)
          Text("share_failed_title").font(.headline)
          Text("share_failed_body").multilineTextAlignment(.center)
        }
      }
      .padding(24)
      .frame(maxWidth: .infinity, maxHeight: .infinity)
      .navigationBarTitleDisplayMode(.inline)
      .toolbar {
        ToolbarItem(placement: .confirmationAction) {
          Button("share_done", action: onDone).disabled(progress.state == .copying)
        }
      }
    }
  }
}

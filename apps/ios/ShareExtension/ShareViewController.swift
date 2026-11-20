import SwiftUI
import UIKit
import UniformTypeIdentifiers
import UserNotifications

/// The share sheet's entry for TeamCraft (issues #208, #1037). It copies what was shared into the
/// App Group and leaves the rest to the app: encryption and the upload need the core, which doesn't
/// belong in an extension's memory limit, and big videos go through here as files only.
///
/// While it copies, it offers the conversations the app listed last (`ShareTargetsSnapshot`):
/// a pick goes with the share, and the app opens that conversation's composer with it. Nothing is
/// sent from here, and nothing before the user pressed send in the app. Without a list (never
/// opened, signed out, app lock on) the app's picker asks, as before. An extension can't open its
/// app, so it says so and posts a notification that does.
final class ShareViewController: UIViewController {
  private let progress = ShareProgress()

  override func viewDidLoad() {
    super.viewDidLoad()
    let maybeInbox = ShareInbox.shared()
    let rows = ShareTargetsStore.shared().read()?.rows(forInboxOwner: maybeInbox?.owner()) ?? []
    progress.targets = rows
    progress.isPicking = !rows.isEmpty
    let host = UIHostingController(
      rootView: ShareStatusView(
        progress: progress,
        onPick: { [weak self] maybeRow in self?.pick(maybeRow) },
        onCancel: { [weak self] in self?.cancel() },
        onDone: { [weak self] in self?.extensionContext?.completeRequest(returningItems: nil) }))
    addChild(host)
    host.view.frame = view.bounds
    host.view.autoresizingMask = [.flexibleWidth, .flexibleHeight]
    view.addSubview(host.view)
    host.didMove(toParent: self)
    // A swipe must not look like sending or discarding: the buttons say which.
    isModalInPresentation = true
    let items = extensionContext?.inputItems.compactMap { $0 as? NSExtensionItem } ?? []
    Task { await stage(items, inbox: maybeInbox) }
  }

  /// Copies everything and commits the share without a conversation at once: should the extension
  /// end before a pick (memory, the user leaving), the app's picker still has it.
  private func stage(_ items: [NSExtensionItem], inbox maybeInbox: ShareInbox?) async {
    guard let inbox = maybeInbox, var share = try? inbox.begin() else {
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
    guard !progress.isCancelled else {
      inbox.delete(share.id)
      return
    }
    guard !share.isEmpty, (try? inbox.commit(share)) != nil else {
      inbox.delete(share.id)
      progress.state = .failed
      return
    }
    progress.staged = share
    progress.state = .copied
    await finishIfChosen()
  }

  /// `nil`: the app's picker chooses ("Choose in TeamCraft").
  private func pick(_ maybeRow: ShareTargetRow?) {
    progress.choice = .some(maybeRow)
    Task { await finishIfChosen() }
  }

  /// Once copied and chosen (or with nothing to choose from), the share is written with its
  /// conversation and caption, and the notification goes out.
  private func finishIfChosen() async {
    guard progress.state == .copied, var share = progress.staged else { return }
    let maybeRow: ShareTargetRow?
    if progress.isPicking {
      guard case .some(let picked) = progress.choice else { return }
      maybeRow = picked
    } else {
      maybeRow = nil
    }
    share = ShareHandoff.apply(destination: maybeRow?.conversationId, caption: progress.caption, to: share)
    if share != progress.staged, let inbox = ShareInbox.shared() {
      // The share without them is already committed; a failure here keeps that one.
      try? inbox.commit(share)
    }
    progress.chosenName = maybeRow?.name
    progress.state = .saved
    await postOpenNotification(hasDestination: maybeRow != nil)
  }

  /// Discards what was copied (also a share already committed without a conversation).
  private func cancel() {
    if let share = progress.staged { ShareInbox.shared()?.delete(share.id) }
    progress.isCancelled = true
    extensionContext?.cancelRequest(withError: NSError(domain: NSCocoaErrorDomain, code: NSUserCancelledError))
  }

  /// A tap opens the app, which finds the share in the inbox. Without notification permission the
  /// sheet's text is what's left, and it says the same. The conversation's name stays out of it:
  /// a notification can show on the lock screen.
  private func postOpenNotification(hasDestination: Bool) async {
    let content = UNMutableNotificationContent()
    content.title = String(localized: "share_saved_title")
    content.body =
      hasDestination ? String(localized: "share_saved_to_notification") : String(localized: "share_saved_notification")
    let request = UNNotificationRequest(identifier: "share-saved", content: content, trigger: nil)
    try? await UNUserNotificationCenter.current().add(request)
  }
}

/// What the sheet shows.
@MainActor
@Observable
final class ShareProgress {
  enum State {
    case copying, copied, saved, failed
  }

  var state = State.copying
  /// The conversations offered; empty without a list from the app.
  var targets: [ShareTargetRow] = []
  /// The picker is up (there is a list); it stays the sheet's content until a choice.
  var isPicking = false
  /// `.none` until a choice; `.some(nil)` is "Choose in TeamCraft".
  var choice: ShareTargetRow?? = .none
  var caption = ""
  var staged: StagedShare?
  var chosenName: String?
  var isCancelled = false
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

  /// The first registered type of one of `kinds`. A Live Photo also registers its bundle (a
  /// folder), which comes last: the still picture goes, and the app takes the picture out of a
  /// bundle that is all there is.
  private static func fileType(of provider: NSItemProvider, among kinds: [UTType]) -> String? {
    let matching = provider.registeredTypeIdentifiers.filter { identifier in
      guard let type = UTType(identifier) else { return false }
      return kinds.contains { type.conforms(to: $0) }
    }
    return matching.first { !isBundle($0) } ?? matching.first
  }

  private static func isBundle(_ identifier: String) -> Bool {
    guard let type = UTType(identifier) else { return false }
    return type.conforms(to: .package) || type.conforms(to: .directory)
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

/// The picker while there is a list and no choice yet; then a plain card: copying, then what
/// happens next.
struct ShareStatusView: View {
  let progress: ShareProgress
  let onPick: (ShareTargetRow?) -> Void
  let onCancel: () -> Void
  let onDone: () -> Void

  var body: some View {
    NavigationStack {
      Group {
        if progress.isPicking && progress.choice == nil && progress.state != .failed {
          SharePicker(progress: progress, onPick: onPick)
        } else {
          card
        }
      }
      .navigationTitle(progress.isPicking && progress.choice == nil ? Text("share_pick_title") : Text(verbatim: ""))
      .navigationBarTitleDisplayMode(.inline)
      .toolbar {
        if progress.state != .saved && progress.state != .failed {
          ToolbarItem(placement: .cancellationAction) {
            Button("share_cancel", role: .cancel, action: onCancel)
          }
        } else {
          ToolbarItem(placement: .confirmationAction) {
            Button("share_done", action: onDone)
          }
        }
      }
    }
  }

  private var card: some View {
    VStack(spacing: 16) {
      switch progress.state {
      case .copying, .copied:
        ProgressView("share_copying")
      case .saved:
        Image(systemName: "checkmark.circle")
          .font(.largeTitle)
          .foregroundStyle(.tint)
          .accessibilityHidden(true)
        Text("share_saved_title").font(.headline)
        if let name = progress.chosenName {
          Text("share_saved_to_body \(name)").multilineTextAlignment(.center)
        } else {
          Text("share_saved_body").multilineTextAlignment(.center)
        }
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
  }
}

/// The conversations the app listed: the notes and the recent ones on top, then all by name,
/// searchable; a caption above, and the app's own picker as a way out.
struct SharePicker: View {
  @Bindable var progress: ShareProgress
  let onPick: (ShareTargetRow?) -> Void
  @State private var filter = ""

  var body: some View {
    List {
      Section {
        TextField("share_caption", text: $progress.caption, axis: .vertical)
          .lineLimit(1...4)
        if progress.state == .copying {
          Label("share_copying", systemImage: "arrow.down.doc")
            .font(.footnote)
            .foregroundStyle(.secondary)
        }
      }
      let sections = ShareTargetSections(rows: progress.targets, filter: filter)
      if !sections.top.isEmpty {
        Section("share_recent") { rows(sections.top) }
      }
      if !sections.others.isEmpty {
        Section("share_all") { rows(sections.others) }
      }
      Section {
        Button {
          onPick(nil)
        } label: {
          Label("share_choose_in_app", systemImage: "arrow.up.forward.app")
        }
      } footer: {
        Text("share_pick_footer")
      }
    }
    .searchable(text: $filter, placement: .navigationBarDrawer(displayMode: .always), prompt: Text("share_filter"))
  }

  private func rows(_ rows: [ShareTargetRow]) -> some View {
    ForEach(rows) { row in
      Button {
        onPick(row)
      } label: {
        HStack(spacing: 12) {
          Image(systemName: row.symbol)
            .foregroundStyle(.tint)
            .frame(width: 24)
            .accessibilityHidden(true)
          VStack(alignment: .leading, spacing: 2) {
            Text(row.name).foregroundStyle(.primary).lineLimit(1)
            if let space = row.maybeSpaceName, !space.isEmpty {
              Text(space).font(.footnote).foregroundStyle(.secondary).lineLimit(1)
            }
          }
        }
      }
    }
  }
}

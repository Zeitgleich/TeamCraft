import Foundation

/// The line a system message shows instead of a bubble (#769), with the events and wording of
/// desktop and web. A change someone made to the conversation (the extras' workspace event) says
/// more than the plain join or leave the core reports with it: "Ada added Sam", not "Sam joined".
enum SystemLine: Equatable {
  case joined(name: String)
  case left(name: String)
  case renamed(name: String, channel: String)
  case topicChanged(name: String, topic: String)
  case memberAdded(name: String, by: String, sharesHistory: Bool)
  case memberRemoved(name: String, by: String)
  // Changes the server reports of the channel (issue #818): `maybeBy` names who made one only when
  // that member's own announcement matched it; otherwise the line is the server's statement ("This
  // channel was archived").
  case archived(maybeBy: String?)
  case unarchived(maybeBy: String?)
  case madePrivate(maybeBy: String?)
  case madePublic(maybeBy: String?)
  case pinned(by: String)
  /// To the space `space`; empty if this device doesn't know its name.
  case moved(maybeBy: String?, space: String)
  /// A workspace admin joined the private channel: by themselves, or let in by the channel owner
  /// `maybeApprovedBy` (admin.md §2.4).
  case adminJoined(name: String, maybeApprovedBy: String?)
  /// A workspace admin joined because `by` added them without an approval to ask for (admin.md
  /// §2.4.2).
  case adminAdded(name: String, by: String)
  /// A message without text or files of a kind this version doesn't know.
  case changed

  /// `nil` for a message people wrote. `names` are display names by user ID, for the people a
  /// workspace event names.
  init?(message: MessageItem, maybeExtras: MessageExtras?, names: [String: String]) {
    // The core makes the person an event is about its author, so their name is known first.
    let nameOf = { (id: String) in names[id] ?? (id == message.authorId ? message.authorName : "") }
    if let event = maybeExtras?.maybeWorkspaceEvent, let line = SystemLine(event, nameOf: nameOf) {
      self = line
    } else if let event = message.maybeSystemEvent {
      self = SystemLine(event, name: message.authorName)
    } else if message.body.isEmpty && (maybeExtras?.files ?? []).isEmpty && maybeExtras?.isDeleted != true {
      self = .changed
    } else {
      return nil
    }
  }

  /// The line in the app's language.
  var text: String {
    switch self {
    case .joined(let name): return String(localized: "event_joined \(name)")
    case .left(let name): return String(localized: "event_left \(name)")
    case .renamed(let name, let channel): return String(localized: "event_renamed \(name) \(channel)")
    case .topicChanged(let name, let topic): return String(localized: "event_topic_changed \(name) \(topic)")
    case .memberAdded(let name, let by, true): return String(localized: "event_member_added_history \(by) \(name)")
    case .memberAdded(let name, let by, false): return String(localized: "event_member_added \(by) \(name)")
    case .memberRemoved(let name, let by): return String(localized: "event_member_removed \(by) \(name)")
    case .archived(let by?): return String(localized: "event_archived \(by)")
    case .archived(nil): return String(localized: "event_archived_unattributed")
    case .unarchived(let by?): return String(localized: "event_unarchived \(by)")
    case .unarchived(nil): return String(localized: "event_unarchived_unattributed")
    case .madePrivate(let by?): return String(localized: "event_made_private \(by)")
    case .madePrivate(nil): return String(localized: "event_made_private_unattributed")
    case .madePublic(let by?): return String(localized: "event_made_public \(by)")
    case .madePublic(nil): return String(localized: "event_made_public_unattributed")
    case .pinned(let by): return String(localized: "event_pinned \(by)")
    case .moved(let by?, let space) where space.isEmpty: return String(localized: "event_moved_unnamed \(by)")
    case .moved(nil, let space) where space.isEmpty: return String(localized: "event_moved_unnamed_unattributed")
    case .moved(let by?, let space): return String(localized: "event_moved \(by) \(space)")
    case .moved(nil, let space): return String(localized: "event_moved_unattributed \(space)")
    case .adminJoined(let name, nil): return String(localized: "event_admin_joined \(name)")
    case .adminJoined(let name, let by?): return String(localized: "event_admin_joined_approved \(name) \(by)")
    case .adminAdded(let name, let by): return String(localized: "event_admin_joined_added \(name) \(by)")
    case .changed: return String(localized: "chat_preview_event")
    }
  }

  private init(_ event: SystemEventItem, name: String) {
    switch event {
    case .joined: self = .joined(name: name)
    case .left: self = .left(name: name)
    case .renamed(let channel): self = .renamed(name: name, channel: channel)
    case .topicChanged(let topic): self = .topicChanged(name: name, topic: topic)
    }
  }

  private init?(_ event: WorkspaceEvent, nameOf: (String) -> String) {
    switch event {
    case .memberAdded(let user, let by, let sharesHistory):
      self = .memberAdded(name: nameOf(user), by: nameOf(by), sharesHistory: sharesHistory)
    case .memberRemoved(let user, let by): self = .memberRemoved(name: nameOf(user), by: nameOf(by))
    case .archived(let maybeBy): self = .archived(maybeBy: maybeBy.map(nameOf))
    case .unarchived(let maybeBy): self = .unarchived(maybeBy: maybeBy.map(nameOf))
    case .madePrivate(let maybeBy): self = .madePrivate(maybeBy: maybeBy.map(nameOf))
    case .madePublic(let maybeBy): self = .madePublic(maybeBy: maybeBy.map(nameOf))
    case .moved(let maybeBy, let space): self = .moved(maybeBy: maybeBy.map(nameOf), space: space)
    case .pinned(let by): self = .pinned(by: nameOf(by))
    case .adminJoined(let user, let maybeApprovedBy):
      self = .adminJoined(name: nameOf(user), maybeApprovedBy: maybeApprovedBy.map(nameOf))
    case .adminAdded(let user, let by): self = .adminAdded(name: nameOf(user), by: nameOf(by))
    case .unknown: return nil
    }
  }
}

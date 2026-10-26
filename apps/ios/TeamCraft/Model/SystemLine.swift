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
  case archived(by: String)
  case unarchived(by: String)
  case madePrivate(by: String)
  case madePublic(by: String)
  case pinned(by: String)
  /// To the space `space`; empty if the mover's device didn't know its name.
  case moved(by: String, space: String)
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
    case .archived(let by): return String(localized: "event_archived \(by)")
    case .unarchived(let by): return String(localized: "event_unarchived \(by)")
    case .madePrivate(let by): return String(localized: "event_made_private \(by)")
    case .madePublic(let by): return String(localized: "event_made_public \(by)")
    case .pinned(let by): return String(localized: "event_pinned \(by)")
    case .moved(let by, let space) where space.isEmpty: return String(localized: "event_moved_unnamed \(by)")
    case .moved(let by, let space): return String(localized: "event_moved \(by) \(space)")
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
    case .archived(let by): self = .archived(by: nameOf(by))
    case .unarchived(let by): self = .unarchived(by: nameOf(by))
    case .madePrivate(let by): self = .madePrivate(by: nameOf(by))
    case .madePublic(let by): self = .madePublic(by: nameOf(by))
    case .moved(let by, let space): self = .moved(by: nameOf(by), space: space)
    case .pinned(let by): self = .pinned(by: nameOf(by))
    case .unknown: return nil
    }
  }
}

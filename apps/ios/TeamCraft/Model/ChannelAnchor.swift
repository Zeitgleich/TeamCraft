import Foundation

/// A channel's name rules and anchor (e2ee.md §4.6.5, issue #1842), as desktop and web show them:
/// what each mark says. The core decides what to say (`teamcraft-ffi`'s `channel_anchor`); the app
/// only picks the strings.
enum ChannelAnchor {
  /// The text at the name field for a name that breaks the rules; an empty one is the form's own.
  static func problemKey(_ problem: ChannelNameProblem) -> String? {
    switch problem {
    case .empty: return nil
    case .bidiControl: return "channel_name_error_bidi"
    case .controlCharacter: return "channel_name_error_control"
    case .invisibleCharacter: return "channel_name_error_invisible"
    }
  }

  /// What the core's check finds wrong with a channel name as it is typed; `nil` when it keeps the
  /// rules.
  static func problem(of name: String) -> ChannelNameProblem? {
    if case .refused(let problem) = checkChannelName(name: name) { return problem }
    return nil
  }

  /// The note's format key and the name it takes, if it names somebody (`nil` inside: nobody can
  /// name them, "Unknown person").
  static func deleteNoteKey(_ note: ChannelDeleteNameNote) -> (key: String, name: String??) {
    switch note {
    case .nameStale: return ("channel_delete_name_stale", nil)
    case .namedByFounder(let name): return ("channel_delete_named_by_founder %@", .some(name))
    case .renamedByWorkspace(let name): return ("channel_delete_renamed_by_workspace %@", .some(name))
    case .renamedByChannelAdmin(let name): return ("channel_delete_renamed_by_channel_admin %@", .some(name))
    case .renamedByMember(let name): return ("channel_delete_renamed_by_member %@", .some(name))
    case .renamedBy(let name): return ("channel_delete_renamed_by %@", .some(name))
    }
  }

  /// The note a channel's deletion confirmation adds about its name, in the user's language.
  static func deleteNote(_ note: ChannelDeleteNameNote) -> String {
    let (key, maybeName) = deleteNoteKey(note)
    let format = String(localized: String.LocalizationValue(key))
    guard let name = maybeName else { return format }
    return String(format: format, name ?? String(localized: "channel_roles_review_unknown"))
  }

  /// A workspace admin's note on a forked anchor, by its side.
  static func forkKey(_ fork: ChannelAnchorForkItem) -> String {
    switch fork {
    case .founding: return "channel_anchor_forked_founding"
    case .properties: return "channel_anchor_forked_properties"
    case .unknown: return "channel_anchor_forked"
    }
  }
}

extension AppModel {
  /// What the channel shows of its anchor; `nil` for a DM, nothing held, or no answer.
  func channelAnchorMarks(_ conversationId: String) async -> ChannelAnchorMarksItem? {
    (try? await core().channelAnchorMarks(conversationId: conversationId)) ?? nil
  }

  /// A channel, its members or its deletion opened: the anchor is verified again (at most every 5
  /// minutes per channel); the marks change with a store update.
  func refreshChannelAnchor(_ conversationId: String) async {
    try? await core().refreshChannelAnchor(conversationId: conversationId)
  }

  /// Whether the channel can't be read on this device: the server offered a group the user's
  /// devices didn't pin for it.
  func isChannelUnreachable(_ conversationId: String) async -> Bool {
    (try? await core().isChannelUnreachable(conversationId: conversationId)) ?? false
  }

  /// The note of a channel's deletion confirmation, verified again first; `nil` when this device
  /// holds nothing verified to say.
  func channelDeleteNote(_ conversationId: String) async -> String? {
    await refreshChannelAnchor(conversationId)
    return await channelAnchorMarks(conversationId)?.maybeDeleteNote.map(ChannelAnchor.deleteNote)
  }
}

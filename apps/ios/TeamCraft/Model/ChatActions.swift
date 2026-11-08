import Foundation

/// The `ChatAction` JSON messages of teamcraft-client-api that the app sends through
/// `TeamCraft.act(actionJson:)` (the same ones the desktop UI sends). Built as dictionaries:
/// the core's serde types are the schema, and ids travel as decimal strings.
enum ChatAction {
  case toggleReaction(conversationId: String, messageId: String, emoji: String)
  case editMessage(conversationId: String, messageId: String, body: String)
  case deleteMessage(conversationId: String, messageId: String)
  case setCustomStatus(CustomStatusValue?)
  case setActivity(isIdle: Bool)
  case sendMessage(SendRequest)
  /// Keeps the message for `dueAtMs` (server time, docs/scheduled-messages.md); it can't have files.
  case scheduleMessage(SendRequest, dueAtMs: Int64)
  case updateScheduled(id: String, body: String, dueAtMs: Int64)
  case cancelScheduled(id: String)
  case sendScheduledNow(id: String)
  case downloadFile(conversationId: String, messageId: String)
  /// One file of a message (the image viewer fetches only the picture on screen).
  case downloadAttachment(conversationId: String, messageId: String, fileId: String)
  /// Unread again from `fromMessageId` on (account data, synced to the user's devices).
  case markUnread(conversationId: String, fromMessageId: String)
  case forward(fromConversationId: String, messageId: String, toConversationId: String)
  /// One file of a message, without the rest of an album (a message with only it goes whole).
  case forwardAttachment(fromConversationId: String, messageId: String, fileId: String, toConversationId: String)
  case setPinned(conversationId: String, messageId: String, isPinned: Bool)
  case setSaved(conversationId: String, messageId: String, isSaved: Bool)
  /// The user types (send while the text changes; the core throttles), in a conversation or with
  /// `maybeThreadId` a reply in one of its threads (#882).
  case typing(conversationId: String, maybeThreadId: String? = nil)
  /// An own message the server rejected: send it again, or drop it.
  case retrySend(conversationId: String, messageId: String)
  case discardUnsent(conversationId: String, messageId: String)
  /// Stops one file of an own message that is still going up; the message goes without it.
  case cancelUpload(conversationId: String, messageId: String, fileId: String)
  case joinChannel(conversationId: String)
  /// Leaves a channel (never a DM); the core sends it once the connection is back.
  case leaveChannel(conversationId: String, deletesIfLast: Bool)
  /// Deletes a channel for everybody, after the user confirmed it.
  case deleteChannel(conversationId: String)
  /// Deletes a space with its channels for everybody, after the user confirmed it.
  case deleteSpace(spaceId: String)
  case createChannel(spaceId: String, name: String)
  /// The direct conversation with a person; created when there is none yet.
  case openDm(userId: String)
  /// The group DM with exactly these people (and the user); created when there is none yet.
  case openGroupDm(userIds: [String])
  /// Reads again how far the others read: receipts sent while the app was offline never arrive.
  case refreshReadReceipts(conversationId: String)
  case refreshSpaceRoster(spaceId: String)
  case refreshCallHistory
  /// The reactions to these own messages were on screen, or the user jumped to them: seen on
  /// every device (issue #1009).
  case markReactionsSeen(conversationId: String, messageIds: [String])
  case loadOlderHistory(conversationId: String)
  /// The reader came near a hole in the history (issue #1438): one page of it, from `edge`.
  case fillHistoryGap(conversationId: String, after: String, edge: GapEdge)
  /// To an absolute time, so a retry doesn't extend twice; the server checks who may (issue #212).
  case extendGuest(spaceId: String, userId: String, expiresAtMs: Int64)
  /// The composer's unsent text, kept by the core (synced to the user's devices) so it survives the
  /// app being killed (issue #158). An empty body clears it.
  case setDraft(conversationId: String, maybeThreadId: String?, body: String)
  /// What the conversation's composer replies to (#1115), kept on this device; nil forgets it.
  case setDraftReply(conversationId: String, reply: ComposerReply?)
  /// Files go up with the draft as they are picked (#1220), each under its `maybeDraftId`, so the
  /// draft keeps them after the app was killed and the user's other devices show and send them.
  case attachDraftFiles(conversationId: String, maybeThreadId: String?, files: [PickedFile])
  /// Takes a file out of the draft on every device of the user; the server deletes it.
  case removeDraftFile(conversationId: String, maybeThreadId: String?, id: String)
  /// Deletes the draft with its files, on every device of the user.
  case discardDraft(conversationId: String, maybeThreadId: String?)

  /// The wire form.
  var json: String {
    var object: [String: Any]
    switch self {
    case .toggleReaction(let conversation, let message, let emoji):
      object = [
        "action": "toggle_reaction", "conversation": conversation, "message": message, "emoji": emoji,
      ]
    case .editMessage(let conversation, let message, let body):
      object = ["action": "edit_message", "conversation": conversation, "message": message, "body": body]
    case .deleteMessage(let conversation, let message):
      object = ["action": "delete_message", "conversation": conversation, "message": message]
    case .setCustomStatus(let maybeStatus):
      object = ["action": "set_custom_status"]
      if let status = maybeStatus {
        object["maybe_status"] = ["emoji": status.emoji, "text": status.text, "expiry": status.expiry.rawValue]
      } else {
        object["maybe_status"] = NSNull()
      }
    case .setActivity(let isIdle):
      object = ["action": "set_activity", "is_idle": isIdle]
    case .sendMessage(let request):
      object = ["action": "send_message", "message": request.object]
    case .downloadFile(let conversation, let message):
      object = ["action": "download_file", "conversation": conversation, "message": message]
    case .downloadAttachment(let conversation, let message, let file):
      object = ["action": "download_attachment", "conversation": conversation, "message": message, "file_id": file]
    case .markUnread(let conversation, let message):
      object = ["action": "mark_unread", "conversation": conversation, "from": message]
    case .forward(let from, let message, let to):
      object = ["action": "forward_message", "from": from, "message": message, "to": to, "comment": ""]
    case .forwardAttachment(let from, let message, let file, let to):
      object = [
        "action": "forward_message", "from": from, "message": message, "to": to, "comment": "", "attachment": file,
      ]
    case .setPinned(let conversation, let message, let isPinned):
      object = ["action": "set_pinned", "conversation": conversation, "message": message, "is_pinned": isPinned]
    case .setSaved(let conversation, let message, let isSaved):
      object = ["action": "set_saved", "conversation": conversation, "message": message, "is_saved": isSaved]
    case .typing(let conversation, let maybeThread):
      object = ["action": "typing", "conversation": conversation]
      if let maybeThread { object["maybe_thread"] = maybeThread }
    case .retrySend(let conversation, let message):
      object = ["action": "retry_send", "conversation": conversation, "message": message]
    case .discardUnsent(let conversation, let message):
      object = ["action": "discard_unsent", "conversation": conversation, "message": message]
    case .cancelUpload(let conversation, let message, let file):
      object = ["action": "cancel_upload", "conversation": conversation, "message": message, "file_id": file]
    case .joinChannel(let conversation):
      object = ["action": "join_channel", "conversation": conversation]
    case .createChannel(let space, let name):
      object = ["action": "create_channel", "space": space, "name": name]
    case .openDm(let user):
      object = ["action": "open_dm", "user": user]
    case .openGroupDm(let users):
      object = ["action": "open_group_dm", "users": users]
    case .refreshSpaceRoster(let space):
      object = ["action": "refresh_space_roster", "space": space]
    case .refreshCallHistory:
      object = ["action": "refresh_call_history"]
    case .refreshReadReceipts(let conversation):
      object = ["action": "refresh_read_receipts", "conversation": conversation]
    case .leaveChannel(let conversation, let deletesIfLast):
      object = ["action": "leave_channel", "conversation": conversation, "deletes_if_last": deletesIfLast]
    case .deleteChannel(let conversation):
      object = ["action": "delete_channel", "conversation": conversation]
    case .deleteSpace(let space):
      object = ["action": "delete_space", "space": space]
    case .extendGuest(let space, let user, let expiresAtMs):
      object = ["action": "extend_guest", "space": space, "user": user, "expires_at_ms": expiresAtMs]
    case .markReactionsSeen(let conversation, let messages):
      object = ["action": "mark_reactions_seen", "conversation": conversation, "messages": messages]
    case .loadOlderHistory(let conversation):
      object = ["action": "load_older_history", "conversation": conversation]
    case .fillHistoryGap(let conversation, let after, let edge):
      object = ["action": "fill_history_gap", "conversation": conversation, "after": after, "edge": edge.rawValue]
    case .scheduleMessage(let request, let dueAtMs):
      object = ["action": "schedule_message", "message": request.object, "due_at": dueAtMs]
    case .updateScheduled(let id, let body, let dueAtMs):
      object = ["action": "update_scheduled", "id": id, "body": body, "due_at": dueAtMs]
    case .cancelScheduled(let id):
      object = ["action": "cancel_scheduled", "id": id]
    case .sendScheduledNow(let id):
      object = ["action": "send_scheduled_now", "id": id]
    case .setDraft(let conversation, let maybeThread, let body):
      object = [
        "action": "set_draft", "conversation": conversation,
        "maybe_thread": maybeThread.map { $0 as Any } ?? NSNull(), "body": body,
      ]
    case .setDraftReply(let conversation, let maybeReply):
      var reply: Any = NSNull()
      if let chosen = maybeReply {
        reply = [
          "message": chosen.messageId,
          "maybe_attachment": chosen.maybeAttachmentId.map { $0 as Any } ?? NSNull(),
        ] as [String: Any]
      }
      object = ["action": "set_draft_reply", "conversation": conversation, "maybe_reply": reply]
    case .attachDraftFiles(let conversation, let maybeThread, let files):
      object = [
        "action": "attach_draft_files", "conversation": conversation,
        "maybe_thread": maybeThread.map { $0 as Any } ?? NSNull(), "files": files.map(\.object),
      ]
    case .removeDraftFile(let conversation, let maybeThread, let id):
      object = [
        "action": "remove_draft_file", "conversation": conversation,
        "maybe_thread": maybeThread.map { $0 as Any } ?? NSNull(), "id": id,
      ]
    case .discardDraft(let conversation, let maybeThread):
      object = [
        "action": "discard_draft", "conversation": conversation,
        "maybe_thread": maybeThread.map { $0 as Any } ?? NSNull(),
      ]
    }
    return jsonText(object)
  }
}

/// A message with files or a reply (`OutgoingMessage` of the core). Plain text goes through
/// `TeamCraft.sendMessage` instead.
struct SendRequest: Equatable {
  var conversationId: String
  var maybeThreadId: String?
  var body: String
  var files: [PickedFile] = []
  /// Files of the draft attached on another device, or before the composer opened (#1220).
  var draftFiles: [DraftFileItem] = []
  var maybeReplyToId: String?
  /// With `maybeReplyToId`: the one picture of an album the reply answers.
  var maybeReplyAttachmentId: String?
  /// Chosen by the composer when a retry must be the same message (a scheduled one).
  var maybeClientMessageId: String?

  var object: [String: Any] {
    let fromDraft: [[String: Any]] = draftFiles.map { item in
      ["file_name": item.fileName, "media_type": item.mediaType, "source": ["kind": "draft", "id": item.id]]
    }
    return [
      "conversation": conversationId,
      "maybe_thread": maybeThreadId.map { $0 as Any } ?? NSNull(),
      "body": body,
      "maybe_reply_to": maybeReplyToId.map { $0 as Any } ?? NSNull(),
      "maybe_reply_attachment": maybeReplyAttachmentId.map { $0 as Any } ?? NSNull(),
      "maybe_client_message_id": maybeClientMessageId.map { $0 as Any } ?? NSNull(),
      "priority": "normal",
      "wants_acknowledgement": false,
      "is_silent": false,
      "also_send_to_channel": false,
      "files": files.map(\.object) + fromDraft,
    ]
  }
}

/// A file picked for sending, already copied to a path the core can read.
struct PickedFile: Equatable, Identifiable {
  let fileName: String
  let mediaType: String
  let path: String
  /// A recorded voice message's length (#204).
  var maybeDurationMs: Int64?
  /// A recorded voice message's waveform, one byte per bar (#204).
  var maybeWaveform: [UInt8]?
  /// Its ID in the composer's draft (#1220), chosen as it is attached: sending it takes the draft's
  /// upload where there is one, and reads `path` otherwise.
  var maybeDraftId: String?

  var id: String { path }

  /// The core's `OutgoingFile`.
  var object: [String: Any] {
    var object: [String: Any] = [
      "file_name": fileName, "media_type": mediaType,
      "source": ["kind": "path", "path": path],
    ]
    // A voice message (#204): its length and waveform travel inside the encrypted message.
    if let duration = maybeDurationMs { object["duration_ms"] = duration }
    if let waveform = maybeWaveform { object["waveform"] = waveform.map { Int($0) } }
    if let draftId = maybeDraftId { object["draft_id"] = draftId }
    return object
  }

  /// A draft's file ID as the core reads it: 32 lowercase hex digits (`ClientMessageId`).
  static func newDraftId() -> String {
    UUID().uuidString.replacingOccurrences(of: "-", with: "").lowercased()
  }
}

/// The `ChatQuery` JSON messages (tagged by `kind`) that `TeamCraft.query(queryJson:)` answers.
enum ChatQuery {
  case messageExtras(conversationId: String, messageIds: [String])
  case conversationFiles(conversationId: String)
  case spaceFiles(spaceId: String)
  case spaceSavedItems(spaceId: String)
  case spaceCallHistory(spaceId: String)
  case spaceMembers(spaceId: String)
  case maxUploadBytes
  case resolvePermalink(link: String)
  /// Everyone typing anywhere, thread replies apart: one answer for the list, the header and the lines (#882).
  case typingNow
  case permalink(conversationId: String, messageId: String)
  /// The channels of a space the user is in or can join.
  case browseChannels(spaceId: String)
  case expiringGuests
  /// Others' reactions to the user's messages here that the user hasn't seen.
  case unseenReactions(conversationId: String)
  case olderHistory(conversationId: String)
  /// The composer text left unsent here, from this or another device of the user.
  case draft(conversationId: String, maybeThreadId: String?)
  /// The messages waiting for their time, and the workspace's rule for them.
  case scheduledMessages
  /// Every draft of the user (#1220).
  case drafts
  /// The files of one draft (#1220), decoded as `DraftFilesAnswer`.
  case draftFiles(conversationId: String, maybeThreadId: String?)
  /// What the conversation's composer replies to, as a quote (#1115).
  case draftReply(conversationId: String)
  /// The user's recent messages across the conversations (#1220).
  case sentMessages

  var json: String {
    switch self {
    case .messageExtras(let conversation, let messages):
      return jsonText(["kind": "message_extras", "conversation": conversation, "messages": messages])
    case .conversationFiles(let conversation):
      return jsonText(["kind": "conversation_files", "conversation": conversation])
    case .spaceFiles(let space):
      return jsonText(["kind": "space_files", "space": space])
    case .spaceSavedItems(let space):
      return jsonText(["kind": "space_saved_items", "space": space])
    case .spaceCallHistory(let space):
      return jsonText(["kind": "space_call_history", "space": space])
    case .spaceMembers(let space):
      return jsonText(["kind": "space_members", "space": space])
    case .maxUploadBytes:
      return jsonText(["kind": "max_upload_bytes"])
    case .resolvePermalink(let link):
      return jsonText(["kind": "resolve_permalink", "link": link])
    case .typingNow:
      return jsonText(["kind": "typing_now"])
    case .permalink(let conversation, let message):
      return jsonText(["kind": "permalink", "conversation": conversation, "message": message])
    case .browseChannels(let space):
      return jsonText(["kind": "browse_channels", "space": space])
    case .expiringGuests:
      return jsonText(["kind": "expiring_guests"])
    case .unseenReactions(let conversation):
      return jsonText(["kind": "unseen_reactions", "conversation": conversation])
    case .olderHistory(let conversation):
      return jsonText(["kind": "older_history", "conversation": conversation])
    case .scheduledMessages:
      return jsonText(["kind": "scheduled_messages"])
    case .draft(let conversation, let maybeThread):
      return jsonText([
        "kind": "draft", "conversation": conversation, "maybe_thread": maybeThread.map { $0 as Any } ?? NSNull(),
      ])
    case .drafts:
      return jsonText(["kind": "drafts"])
    case .draftReply(let conversation):
      return jsonText(["kind": "draft_reply", "conversation": conversation])
    case .draftFiles(let conversation, let maybeThread):
      return jsonText([
        "kind": "draft_files", "conversation": conversation,
        "maybe_thread": maybeThread.map { $0 as Any } ?? NSNull(),
      ])
    case .sentMessages:
      return jsonText(["kind": "sent_messages"])
    }
  }
}

/// Compact JSON with sorted keys.
func jsonText(_ object: [String: Any]) -> String {
  // Invariant: callers pass only strings, numbers, bools, nulls and arrays or dictionaries of them,
  // which always serialize.
  let data = (try? JSONSerialization.data(withJSONObject: object, options: [.sortedKeys])) ?? Data("{}".utf8)
  return String(decoding: data, as: UTF8.self)
}

/// A custom status next to the user's name ("🌴 Vacation").
struct CustomStatusValue: Equatable {
  var emoji: String
  var text: String
  var expiry: StatusExpiryValue
}

/// When a custom status clears itself (the core's `StatusExpiry` variant names).
enum StatusExpiryValue: String, CaseIterable, Identifiable {
  case never = "Never"
  case thirtyMinutes = "ThirtyMinutes"
  case oneHour = "OneHour"
  case today = "Today"
  case thisWeek = "ThisWeek"

  var id: String { rawValue }
}

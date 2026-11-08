package de.zeitgleich.teamcraft.core

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObjectBuilder
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.add
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

/**
 * A file picked for sending, already copied to a path the core can read. A recorded voice message
 * (#204) also carries its length and waveform, which travel inside the encrypted message.
 */
data class PickedFile(
  val fileName: String,
  val mediaType: String,
  val path: String,
  /** A video's poster frame (JPEG), made by the app: the core can't decode videos. */
  val maybeThumbnail: ByteArray? = null,
  val maybeDurationMs: Long? = null,
  val maybeWaveform: List<Int>? = null,
  /** In the composer's draft (#1220): it went up with the draft, and its message takes it from there. */
  val maybeDraft: DraftCopy? = null,
)

/**
 * A picked file as the draft holds it (#1220): [id] is the app's choice ([DraftCopy.newId]), and
 * [path] the core's own name of the staged copy, which the core deletes once the file is up. The
 * composer keeps [PickedFile.path] for the preview and the picture editor.
 */
data class DraftCopy(val id: String, val path: String) {
  companion object {
    /** A draft's file ID as the core reads it: 32 lowercase hex digits (`ClientMessageId`). */
    fun newId(): String = java.util.UUID.randomUUID().toString().replace("-", "")
  }
}

/** A picture added to a problem report. */
class ReportScreenshot(val fileName: String, val mediaType: String, val bytes: ByteArray)

/** What the composer sends besides the text. */
data class SendOptions(
  val maybeThread: String? = null,
  val maybeReplyTo: String? = null,
  /** With [maybeReplyTo]: the one picture of an album the reply answers. */
  val maybeReplyAttachment: String? = null,
  /** `normal`, `important` or `urgent`. */
  val priority: String = "normal",
  val wantsAcknowledgement: Boolean = false,
  val isSilent: Boolean = false,
  val alsoSendToChannel: Boolean = false,
  val files: List<PickedFile> = emptyList(),
  /** Files of the draft attached on another device, or before the composer opened (#1220). */
  val draftFiles: List<DraftFileItem> = emptyList(),
  /** Chosen by the composer when a retry must be the same message (a scheduled one). */
  val maybeClientMessageId: String? = null,
)

/**
 * Builds the core's `ChatAction` messages (teamcraft-client-api `remote`, tagged by `action`).
 * Only shapes data: the core decides what an action does.
 */
object ChatActions {
  fun send(conversation: String, body: String, options: SendOptions = SendOptions()): String = action("send_message") {
    put("message", outgoing(conversation, body, options))
  }

  /** Keeps the message for [dueAtMs] (server time, docs/scheduled-messages.md); it can't have files. */
  fun schedule(conversation: String, body: String, options: SendOptions, dueAtMs: Long) = action("schedule_message") {
    put("message", outgoing(conversation, body, options))
    put("due_at", dueAtMs)
  }

  fun updateScheduled(id: String, body: String, dueAtMs: Long) = action("update_scheduled") {
    put("id", id)
    put("body", body)
    put("due_at", dueAtMs)
  }

  fun cancelScheduled(id: String) = action("cancel_scheduled") { put("id", id) }

  fun sendScheduledNow(id: String) = action("send_scheduled_now") { put("id", id) }

  /**
   * "Report a problem" (docs/bug-reports.md): [id] is the report's client message ID (32 hex
   * digits), so a retried send is delivered once; [reportText] is exactly what the screen showed.
   */
  fun sendBugReport(
    id: String,
    description: String,
    reportText: String,
    fileName: String,
    maybeScreenshot: ReportScreenshot?,
  ) = action("send_bug_report") {
    put(
      "report",
      buildJsonObject {
        put("id", id)
        put("description", description)
        put("report_text", reportText)
        put("file_name", fileName)
        maybeScreenshot?.let { screenshot ->
          put(
            "screenshot",
            buildJsonObject {
              put("file_name", screenshot.fileName)
              put("media_type", screenshot.mediaType)
              put("bytes", java.util.Base64.getEncoder().encodeToString(screenshot.bytes))
            },
          )
        }
      },
    )
  }

  private fun outgoing(conversation: String, body: String, options: SendOptions) = buildJsonObject {
    put("conversation", conversation)
    options.maybeClientMessageId?.let { put("maybe_client_message_id", it) }
    put("maybe_thread", options.maybeThread.json())
    put("body", body)
    put("maybe_reply_to", options.maybeReplyTo.json())
    put("maybe_reply_attachment", options.maybeReplyAttachment.json())
    put("priority", options.priority)
    put("wants_acknowledgement", options.wantsAcknowledgement)
    put("is_silent", options.isSilent)
    put("also_send_to_channel", options.alsoSendToChannel)
    put(
      "files",
      buildJsonArray {
        options.files.forEach { add(outgoingFile(it)) }
        options.draftFiles.forEach { item ->
          add(
            buildJsonObject {
              put("file_name", item.fileName)
              put("media_type", item.mediaType)
              put("source", buildJsonObject { put("kind", "draft"); put("id", item.id) })
            },
          )
        }
      },
    )
  }

  /**
   * A picked file: one of the draft is read from the core's copy, and goes with the draft's upload
   * where there is one (the core reads the file only when the attachment hasn't reached it).
   */
  private fun outgoingFile(file: PickedFile) = buildJsonObject {
    put("file_name", file.fileName)
    put("media_type", file.mediaType)
    put("source", buildJsonObject { put("kind", "path"); put("path", file.maybeDraft?.path ?: file.path) })
    file.maybeThumbnail?.let { bytes -> put("thumbnail", buildJsonArray { bytes.forEach { add(it.toInt() and 0xFF) } }) }
    file.maybeDurationMs?.let { put("duration_ms", it) }
    file.maybeWaveform?.let { waveform -> put("waveform", buildJsonArray { waveform.forEach { add(JsonPrimitive(it)) } }) }
    file.maybeDraft?.let { put("draft_id", it.id) }
  }

  fun react(conversation: String, message: String, emoji: String) = action("toggle_reaction") {
    put("conversation", conversation)
    put("message", message)
    put("emoji", emoji)
  }

  fun edit(conversation: String, message: String, body: String) = action("edit_message") {
    put("conversation", conversation)
    put("message", message)
    put("body", body)
  }

  fun delete(conversation: String, message: String) = action("delete_message") {
    put("conversation", conversation)
    put("message", message)
  }

  /** Forwards the message, or with [maybeAttachment] that file alone (a message with only it goes whole). */
  fun forward(from: String, message: String, to: String, comment: String, maybeAttachment: String? = null) = action("forward_message") {
    put("from", from)
    put("message", message)
    put("to", to)
    put("comment", comment)
    maybeAttachment?.let { put("attachment", it) }
  }

  fun setPinned(conversation: String, message: String, isPinned: Boolean) = action("set_pinned") {
    put("conversation", conversation)
    put("message", message)
    put("is_pinned", isPinned)
  }

  fun setSaved(conversation: String, message: String, isSaved: Boolean) = action("set_saved") {
    put("conversation", conversation)
    put("message", message)
    put("is_saved", isSaved)
  }

  fun markRead(conversation: String) = action("mark_read") { put("conversation", conversation) }

  fun refreshSpaceRoster(space: String) = action("refresh_space_roster") { put("space", space) }

  fun refreshCallHistory() = action("refresh_call_history") {}

  /** The user pressed "Try again" on a list that could not be read ([ChatQueries.listLoad]). */
  fun retryList(list: LoadingList) = action("retry_list") {
    put("list", buildJsonObject { put("name", list.wire) })
  }

  /** The reactions to these own messages were on screen (or jumped to): seen, on every device. */
  fun markReactionsSeen(conversation: String, messages: List<String>) = action("mark_reactions_seen") {
    put("conversation", conversation)
    put("messages", JsonArray(messages.map(::JsonPrimitive)))
  }

  /** The reader came near the oldest row held: one more page from the server. */
  fun loadOlderHistory(conversation: String) = action("load_older_history") { put("conversation", conversation) }

  /** Reads again how far the others read: receipts sent while the app was offline never arrive. */
  fun refreshReadReceipts(conversation: String) = action("refresh_read_receipts") { put("conversation", conversation) }

  fun markUnread(conversation: String, from: String) = action("mark_unread") {
    put("conversation", conversation)
    put("from", from)
  }

  fun setDraft(conversation: String, maybeThread: String?, body: String) = action("set_draft") {
    put("conversation", conversation)
    put("maybe_thread", maybeThread.json())
    put("body", body)
  }

  /** Files go up with the draft as they are picked (#1220); each carries its [DraftCopy]. */
  fun attachDraftFiles(conversation: String, maybeThread: String?, files: List<PickedFile>) = action("attach_draft_files") {
    put("conversation", conversation)
    put("maybe_thread", maybeThread.json())
    put("files", buildJsonArray { files.forEach { add(outgoingFile(it)) } })
  }

  /** Takes a file out of the draft on every device of the user; the server deletes it. */
  fun removeDraftFile(conversation: String, maybeThread: String?, id: String) = action("remove_draft_file") {
    put("conversation", conversation)
    put("maybe_thread", maybeThread.json())
    put("id", id)
  }

  /** Deletes the draft with its files, on every device of the user. */
  fun discardDraft(conversation: String, maybeThread: String?) = action("discard_draft") {
    put("conversation", conversation)
    put("maybe_thread", maybeThread.json())
  }

  /** The user types in a conversation, or with [maybeThread] a reply in one of its threads (#882). */
  fun typing(conversation: String, maybeThread: String? = null) = action("typing") {
    put("conversation", conversation)
    put("maybe_thread", maybeThread.json())
  }

  fun retrySend(conversation: String, message: String) = action("retry_send") {
    put("conversation", conversation)
    put("message", message)
  }

  fun discardUnsent(conversation: String, message: String) = action("discard_unsent") {
    put("conversation", conversation)
    put("message", message)
  }

  /** Stops one file of an own message that is still going up; the message goes without it. */
  fun cancelUpload(conversation: String, message: String, fileId: String) = action("cancel_upload") {
    put("conversation", conversation)
    put("message", message)
    put("file_id", fileId)
  }

  fun acknowledge(conversation: String, message: String) = action("acknowledge") {
    put("conversation", conversation)
    put("message", message)
  }

  fun download(conversation: String, message: String) = action("download_file") {
    put("conversation", conversation)
    put("message", message)
  }

  /** One file of a message: the image viewer fetches only the picture on screen. */
  fun downloadAttachment(conversation: String, message: String, fileId: String) = action("download_attachment") {
    put("conversation", conversation)
    put("message", message)
    put("file_id", fileId)
  }

  fun setFavorite(conversation: String, isFavorite: Boolean) = action("set_favorite") {
    put("conversation", conversation)
    put("is_favorite", isFavorite)
  }

  /** `Online`, `Away`, `DoNotDisturb` or `Offline`. */
  fun setPresence(presence: String) = action("set_presence") { put("presence", presence) }

  /** `expiry`: `Never`, `ThirtyMinutes`, `OneHour`, `Today` or `ThisWeek`; no status clears it. */
  fun setCustomStatus(maybeStatus: CustomStatus?) = action("set_custom_status") {
    put(
      "maybe_status",
      maybeStatus?.let {
        buildJsonObject {
          put("emoji", it.emoji)
          put("text", it.text)
          put("expiry", it.expiry)
        }
      } ?: JsonNull,
    )
  }

  fun setThreadFollowed(conversation: String, thread: String, isFollowed: Boolean) = action("set_thread_followed") {
    put("conversation", conversation)
    put("thread", thread)
    put("is_followed", isFollowed)
  }

  fun markThreadRead(conversation: String, thread: String) = action("mark_thread_read") {
    put("conversation", conversation)
    put("thread", thread)
  }

  fun joinChannel(conversation: String) = action("join_channel") { put("conversation", conversation) }

  /** [deletesIfLast]: the user confirmed that this deletes a channel nobody else is in. */
  fun leaveChannel(conversation: String, deletesIfLast: Boolean = false) = action("leave_channel") {
    put("conversation", conversation)
    put("deletes_if_last", deletesIfLast)
  }

  fun deleteChannel(conversation: String) = action("delete_channel") { put("conversation", conversation) }

  fun deleteSpace(space: String) = action("delete_space") { put("space", space) }

  /** `update_channel` with only `maybe_is_archived` set; the other changes stay absent. */
  fun archiveChannel(conversation: String, isArchived: Boolean = true) = action("update_channel") {
    put("conversation", conversation)
    put("changes", buildJsonObject { put("maybe_is_archived", isArchived) })
  }

  /** To an absolute time, so a retry doesn't extend twice; the server checks who may. */
  fun extendGuest(space: String, user: String, expiresAtMs: Long) = action("extend_guest") {
    put("space", space)
    put("user", user)
    put("expires_at_ms", expiresAtMs)
  }

  fun createChannel(space: String, name: String) = action("create_channel") {
    put("space", space)
    put("name", name)
  }

  fun openDm(user: String) = action("open_dm") { put("user", user) }

  fun openGroupDm(users: List<String>) = action("open_group_dm") { put("users", buildJsonArray { users.forEach { add(JsonPrimitive(it)) } }) }

  fun changePassphrase(passphrase: String) = action("change_passphrase") { put("passphrase", passphrase) }

  fun acceptKeyChange(user: String) = action("accept_key_change") { put("user", user) }

  private fun action(name: String, fields: JsonObjectBuilder.() -> Unit): String =
    buildJsonObject {
      put("action", name)
      fields()
    }.toString()
}

/** The lists whose status the core tracks (`LoadingList`); the others are read from the store. */
enum class LoadingList(val wire: String) {
  CALL_HISTORY("call_history"),
  ACTIVITY("activity"),
}

/** Builds the core's `ChatQuery` messages (tagged by `kind`). */
object ChatQueries {
  fun messageExtras(conversation: String, messages: List<String>) = query("message_extras") {
    put("conversation", conversation)
    put("messages", JsonArray(messages.map(::JsonPrimitive)))
  }

  fun thread(conversation: String, thread: String) = query("thread") {
    put("conversation", conversation)
    put("thread", thread)
  }

  fun isThreadFollowed(conversation: String, thread: String) = query("is_thread_followed") {
    put("conversation", conversation)
    put("thread", thread)
  }

  fun followedThreads() = query("followed_threads") {}

  /** How far a list is (`ListLoad`): loading, ready or failed. */
  fun listLoad(list: LoadingList) = query("list_load") {
    put("list", buildJsonObject { put("name", list.wire) })
  }

  fun pinned(conversation: String) = query("pinned_messages") { put("conversation", conversation) }

  fun savedItems() = query("saved_items") {}

  fun spaceSavedItems(space: String) = query("space_saved_items") { put("space", space) }

  fun spaceFiles(space: String) = query("space_files") { put("space", space) }

  fun spaceCallHistory(space: String) = query("space_call_history") { put("space", space) }

  fun spaceMembers(space: String) = query("space_members") { put("space", space) }

  fun scheduledMessages() = query("scheduled_messages") {}

  fun permalink(conversation: String, message: String) = query("permalink") {
    put("conversation", conversation)
    put("message", message)
  }

  fun resolvePermalink(link: String) = query("resolve_permalink") { put("link", link) }

  /** Everyone typing anywhere, thread replies apart: one answer for the list, the header and the lines (#882). */
  fun typingNow() = query("typing_now") {}

  fun firstUnread(conversation: String) = query("first_unread") { put("conversation", conversation) }

  fun unseenReactions(conversation: String) = query("unseen_reactions") { put("conversation", conversation) }

  fun olderHistory(conversation: String) = query("older_history") { put("conversation", conversation) }

  fun draft(conversation: String, maybeThread: String?) = query("draft") {
    put("conversation", conversation)
    put("maybe_thread", maybeThread.json())
  }

  fun drafts() = query("drafts") {}

  /** The files of one draft ([decodeDraftFiles]). */
  fun draftFiles(conversation: String, maybeThread: String?) = query("draft_files") {
    put("conversation", conversation)
    put("maybe_thread", maybeThread.json())
  }

  fun sentMessages() = query("sent_messages") {}

  fun files(conversation: String) = query("conversation_files") { put("conversation", conversation) }

  fun maxUploadBytes() = query("max_upload_bytes") {}

  fun browseChannels(space: String) = query("browse_channels") { put("space", space) }

  fun expiringGuests() = query("expiring_guests") {}

  fun search(text: String) = query("search") { put("query", text) }

  fun recentMentions() = query("recent_mentions") {}

  private fun query(kind: String, fields: JsonObjectBuilder.() -> Unit): String =
    buildJsonObject {
      put("kind", kind)
      fields()
    }.toString()
}

private fun String?.json() = this?.let(::JsonPrimitive) ?: JsonNull

/** Decodes an answer of [ChatQueries]. */
fun decodeAnswer(json: String): ChatAnswer = WireJson.decodeFromString(ChatAnswer.serializer(), json)

/** Decodes the answer of [ChatQueries.draftFiles], whose `files` aren't a message's ([ChatAnswer.files]). */
fun decodeDraftFiles(json: String): List<DraftFileItem> = WireJson.decodeFromString(DraftFilesAnswer.serializer(), json).files

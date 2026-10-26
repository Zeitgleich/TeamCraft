package de.zeitgleich.teamcraft.core

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObjectBuilder
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

/** A file picked for sending, already copied to a path the core can read. */
data class PickedFile(val fileName: String, val mediaType: String, val path: String)

/** What the composer sends besides the text. */
data class SendOptions(
  val maybeThread: String? = null,
  val maybeReplyTo: String? = null,
  /** `normal`, `important` or `urgent`. */
  val priority: String = "normal",
  val wantsAcknowledgement: Boolean = false,
  val isSilent: Boolean = false,
  val alsoSendToChannel: Boolean = false,
  val files: List<PickedFile> = emptyList(),
)

/**
 * Builds the core's `ChatAction` messages (teamcraft-client-api `remote`, tagged by `action`).
 * Only shapes data: the core decides what an action does.
 */
object ChatActions {
  fun send(conversation: String, body: String, options: SendOptions = SendOptions()): String = action("send_message") {
    put(
      "message",
      buildJsonObject {
        put("conversation", conversation)
        put("maybe_thread", options.maybeThread.json())
        put("body", body)
        put("maybe_reply_to", options.maybeReplyTo.json())
        put("priority", options.priority)
        put("wants_acknowledgement", options.wantsAcknowledgement)
        put("is_silent", options.isSilent)
        put("also_send_to_channel", options.alsoSendToChannel)
        put(
          "files",
          buildJsonArray {
            options.files.forEach { file ->
              add(
                buildJsonObject {
                  put("file_name", file.fileName)
                  put("media_type", file.mediaType)
                  put("source", buildJsonObject { put("kind", "path"); put("path", file.path) })
                },
              )
            }
          },
        )
      },
    )
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

  fun forward(from: String, message: String, to: String, comment: String) = action("forward_message") {
    put("from", from)
    put("message", message)
    put("to", to)
    put("comment", comment)
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

  fun typing(conversation: String) = action("typing") { put("conversation", conversation) }

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

  fun leaveChannel(conversation: String) = action("leave_channel") { put("conversation", conversation) }

  /** `update_channel` with only `maybe_is_archived` set; the other changes stay absent. */
  fun archiveChannel(conversation: String, isArchived: Boolean = true) = action("update_channel") {
    put("conversation", conversation)
    put("changes", buildJsonObject { put("maybe_is_archived", isArchived) })
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

  fun pinned(conversation: String) = query("pinned_messages") { put("conversation", conversation) }

  fun savedItems() = query("saved_items") {}

  fun permalink(conversation: String, message: String) = query("permalink") {
    put("conversation", conversation)
    put("message", message)
  }

  fun resolvePermalink(link: String) = query("resolve_permalink") { put("link", link) }

  fun typingUsers(conversation: String) = query("typing_users") { put("conversation", conversation) }

  fun firstUnread(conversation: String) = query("first_unread") { put("conversation", conversation) }

  fun draft(conversation: String, maybeThread: String?) = query("draft") {
    put("conversation", conversation)
    put("maybe_thread", maybeThread.json())
  }

  fun files(conversation: String) = query("conversation_files") { put("conversation", conversation) }

  fun maxUploadBytes() = query("max_upload_bytes") {}

  fun browseChannels(space: String) = query("browse_channels") { put("space", space) }

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

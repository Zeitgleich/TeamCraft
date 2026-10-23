package de.zeitgleich.teamcraft.core

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.jsonObject
import de.zeitgleich.teamcraft.notifications.DndSchedule

/**
 * The core's view models as they arrive over the JSON bridge (teamcraft-client-api `model`,
 * `actions` and `remote`). Plain data: every decision about what they mean stays in the core.
 * Fields the app doesn't show are left out; unknown fields are ignored.
 */
val WireJson = Json {
  ignoreUnknownKeys = true
  explicitNulls = false
  encodeDefaults = true
}

@Serializable
data class CustomStatus(val emoji: String, val text: String, val expiry: String = "Today")

@Serializable
data class User(
  val id: String,
  val username: String,
  @SerialName("display_name") val displayName: String,
  val presence: String = "Offline",
  @SerialName("maybe_custom_status") val maybeCustomStatus: CustomStatus? = null,
  @SerialName("avatar_hue") val avatarHue: Int = 0,
  @SerialName("is_guest") val isGuest: Boolean = false,
  @SerialName("is_bot") val isBot: Boolean = false,
)

@Serializable
data class Space(
  val id: String,
  val name: String,
  @SerialName("unread_count") val unreadCount: Int = 0,
  @SerialName("mention_count") val mentionCount: Int = 0,
)

@Serializable
data class Conversation(
  val id: String,
  @SerialName("maybe_space") val maybeSpace: String? = null,
  /** `Public`, `Private`, `Direct`, `Group` or `Integration`. */
  val kind: String,
  val name: String,
  val topic: String = "",
  @SerialName("member_ids") val memberIds: List<String> = emptyList(),
  @SerialName("unread_count") val unreadCount: Int = 0,
  @SerialName("mention_count") val mentionCount: Int = 0,
  @SerialName("is_favorite") val isFavorite: Boolean = false,
  @SerialName("is_muted") val isMuted: Boolean = false,
  @SerialName("can_post") val canPost: Boolean = true,
  val bookmarks: List<Bookmark> = emptyList(),
) {
  val isDirect: Boolean get() = kind == "Direct" || kind == "Group"
}

/** A link or file pinned to a conversation's header. */
@Serializable
data class Bookmark(
  val emoji: String = "",
  val title: String,
  /** `Link` or `File`. */
  val kind: String,
  /** The link of a `Link` bookmark; empty for files. */
  val url: String = "",
  /** The message whose file a `File` bookmark opens. */
  @SerialName("maybe_message") val maybeMessage: String? = null,
) {
  val isLink: Boolean get() = kind == "Link"
}

@Serializable
data class Reaction(val emoji: String, val count: Int, @SerialName("reacted_by_me") val reactedByMe: Boolean = false)

@Serializable
data class Message(
  val id: String,
  val conversation: String,
  val author: String,
  /** `"User"`, `{"System": …}` or `{"Log": …}`. */
  val kind: JsonElement = JsonPrimitive("User"),
  @SerialName("sent_at") val sentAt: Long,
  val body: String,
  @SerialName("is_edited") val isEdited: Boolean = false,
  val reactions: List<Reaction> = emptyList(),
  @SerialName("reply_count") val replyCount: Int = 0,
  @SerialName("maybe_thread") val maybeThread: String? = null,
) {
  /** Outbox messages carry the pending bit in their ID until the server accepted them. */
  val isPending: Boolean get() = (id.toLongOrNull() ?: 0L) and PENDING_ID_BIT != 0L

  /** A join, leave, rename or topic change. */
  val maybeSystemEvent: JsonElement?
    get() = (kind as? JsonObject)?.get("System")

  companion object {
    const val PENDING_ID_BIT = 1L shl 62
  }
}

@Serializable
data class SecuritySnapshot(
  @SerialName("my_fingerprint") val myFingerprint: String = "",
  val contacts: List<ContactKeys> = emptyList(),
)

@Serializable
data class ContactKeys(
  val user: String,
  val fingerprint: String,
  @SerialName("maybe_new_fingerprint") val maybeNewFingerprint: String? = null,
)

@Serializable
data class ChatSnapshot(
  val me: User,
  val users: List<User> = emptyList(),
  val spaces: List<Space> = emptyList(),
  val conversations: List<Conversation> = emptyList(),
  val security: SecuritySnapshot = SecuritySnapshot(),
) {
  fun user(id: String): User? = users.firstOrNull { it.id == id } ?: me.takeIf { it.id == id }
  fun conversation(id: String): Conversation? = conversations.firstOrNull { it.id == id }
  fun name(userId: String): String = user(userId)?.displayName ?: userId
}

@Serializable
data class MessageWindow(val conversation: String, val start: Int, val total: Int, val messages: List<Message>)

@Serializable
data class ReplyPreview(
  @SerialName("maybe_message") val maybeMessage: String? = null,
  val author: String,
  val excerpt: String,
)

@Serializable
data class ForwardedFrom(
  @SerialName("author_name") val authorName: String,
  @SerialName("maybe_conversation_name") val maybeConversationName: String? = null,
  @SerialName("sent_at") val sentAt: Long,
)

@Serializable
data class MessageEdit(val body: String, @SerialName("replaced_at") val replacedAt: Long)

/** Where a file stands: `remote`, `uploading`, `downloading`, `local` or `failed`. */
@Serializable
data class FileTransfer(
  val state: String,
  @SerialName("done_bytes") val doneBytes: Long = 0,
  @SerialName("total_bytes") val totalBytes: Long = 0,
  val path: String? = null,
  val reason: String? = null,
) {
  /** 0..1 while uploading or downloading. */
  val maybeProgress: Float?
    get() = if ((state == "uploading" || state == "downloading") && totalBytes > 0) doneBytes.toFloat() / totalBytes else null
}

@Serializable
data class FileInfo(
  val conversation: String,
  val message: String,
  @SerialName("file_id") val fileId: String,
  @SerialName("file_name") val fileName: String,
  @SerialName("media_type") val mediaType: String,
  @SerialName("size_bytes") val sizeBytes: Long,
  @SerialName("maybe_thumbnail_png") val maybeThumbnailPng: List<Int>? = null,
  val transfer: FileTransfer,
) {
  val isImage: Boolean get() = mediaType.startsWith("image/")
  fun thumbnailBytes(): ByteArray? = maybeThumbnailPng?.let { bytes -> ByteArray(bytes.size) { bytes[it].toByte() } }
}

@Serializable
data class LinkPreview(
  val url: String,
  @SerialName("maybe_title") val maybeTitle: String? = null,
  @SerialName("maybe_description") val maybeDescription: String? = null,
  @SerialName("maybe_site_name") val maybeSiteName: String? = null,
  @SerialName("maybe_image") val maybeImage: List<Int>? = null,
) {
  fun imageBytes(): ByteArray? = maybeImage?.let { bytes -> ByteArray(bytes.size) { bytes[it].toByte() } }
}

@Serializable
data class MessageExtras(
  val message: String,
  @SerialName("maybe_reply") val maybeReply: ReplyPreview? = null,
  @SerialName("maybe_forwarded") val maybeForwarded: ForwardedFrom? = null,
  val priority: String = "normal",
  @SerialName("wants_acknowledgement") val wantsAcknowledgement: Boolean = false,
  @SerialName("acknowledged_by") val acknowledgedBy: List<String> = emptyList(),
  @SerialName("is_pinned") val isPinned: Boolean = false,
  @SerialName("is_saved") val isSaved: Boolean = false,
  @SerialName("is_silent") val isSilent: Boolean = false,
  @SerialName("is_deleted") val isDeleted: Boolean = false,
  val edits: List<MessageEdit> = emptyList(),
  val files: List<FileInfo> = emptyList(),
  @SerialName("maybe_link_preview") val maybeLinkPreview: LinkPreview? = null,
  @SerialName("maybe_workspace_event") val maybeWorkspaceEvent: JsonObject? = null,
)

@Serializable
data class ThreadSummary(
  val conversation: String,
  val root: Message,
  @SerialName("reply_count") val replyCount: Int = 0,
  @SerialName("unread_replies") val unreadReplies: Int = 0,
  @SerialName("last_reply_at") val lastReplyAt: Long = 0,
  @SerialName("has_mention") val hasMention: Boolean = false,
)

@Serializable
data class SavedItem(
  val message: Message,
  @SerialName("conversation_name") val conversationName: String,
  @SerialName("saved_at") val savedAt: Long,
)

@Serializable
data class BrowsableChannel(
  val conversation: String,
  val space: String,
  val name: String,
  val topic: String = "",
  @SerialName("is_member") val isMember: Boolean = false,
  @SerialName("is_archived") val isArchived: Boolean = false,
  @SerialName("has_external_people") val hasExternalPeople: Boolean = false,
)

@Serializable
data class PermalinkTarget(
  val conversation: String,
  val message: String,
  @SerialName("maybe_thread") val maybeThread: String? = null,
)

@Serializable
data class SearchHit(
  val message: Message,
  @SerialName("conversation_name") val conversationName: String = "",
)

@Serializable
data class SearchResults(val hits: List<SearchHit> = emptyList())

/** The tagged answer of a query; read the field that belongs to its `answer`. */
@Serializable
data class ChatAnswer(
  val answer: String,
  val extras: List<MessageExtras> = emptyList(),
  val messages: List<Message> = emptyList(),
  val items: List<SavedItem> = emptyList(),
  @SerialName("maybe_link") val maybeLink: String? = null,
  @SerialName("maybe_target") val maybeTarget: PermalinkTarget? = null,
  @SerialName("maybe_message") val maybeMessage: String? = null,
  @SerialName("maybe_body") val maybeBody: String? = null,
  val files: List<FileInfo> = emptyList(),
  @SerialName("maybe_bytes") val maybeBytes: Long? = null,
  val channels: List<BrowsableChannel> = emptyList(),
  val hits: List<SearchHit> = emptyList(),
  val results: SearchResults? = null,
  val value: Boolean = false,
  val threads: List<ThreadSummary> = emptyList(),
  val users: List<String> = emptyList(),
)

/** Global notification preferences the server enforces (`/me/notification-settings`). */
@Serializable
data class NotificationSettings(
  @SerialName("auto_follow_mentioned_threads") val autoFollowMentionedThreads: Boolean = true,
  @SerialName("push_mode") val pushMode: String = "away_only",
  @SerialName("send_read_receipts") val sendReadReceipts: Boolean = true,
  @SerialName("dnd_schedule") val maybeDndSchedule: DndSchedule? = null,
)

/** Per-conversation level (`all`, `mentions`, `none`) and mute time. */
@Serializable
data class ConversationNotificationSettings(
  val level: String = "all",
  @SerialName("muted_until") val mutedUntil: Long? = null,
  /** Follows the space's setting; saving a level or mute makes it the conversation's own. */
  @SerialName("is_inherited") val isInherited: Boolean = false,
)

/** `PUT /me/presence`: `dnd` (optionally until a time) or `auto`. */
@Serializable
data class ManualPresence(
  val manual: String,
  @SerialName("until") val maybeUntil: Long? = null,
)

/** One user's presence (`GET /presence?user_ids=`). */
@Serializable
data class UserPresence(@SerialName("user_id") val userId: String, val state: String)

@Serializable
data class PresenceList(val presences: List<UserPresence> = emptyList())

@Serializable
data class Meeting(
  val id: String,
  val title: String,
  @SerialName("starts_at") val startsAt: Long,
  @SerialName("ends_at") val endsAt: Long,
  val status: String = "",
  @SerialName("conversation_id") val conversationId: String,
)

@Serializable
data class MeetingList(val meetings: List<Meeting> = emptyList())

/** Reads one string field of a JSON object answer, for small replies. */
fun JsonElement.stringField(name: String): String? = (jsonObject[name] as? JsonPrimitive)?.content

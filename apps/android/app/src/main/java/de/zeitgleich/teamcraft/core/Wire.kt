package de.zeitgleich.teamcraft.core

import kotlinx.serialization.ExperimentalSerializationApi
import kotlinx.serialization.KSerializer
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.builtins.IntArraySerializer
import kotlinx.serialization.descriptors.SerialDescriptor
import kotlinx.serialization.encoding.Decoder
import kotlinx.serialization.encoding.Encoder
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.jsonObject
import de.zeitgleich.teamcraft.notifications.DndSchedule
import de.zeitgleich.teamcraft.notifications.DndWindow

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
  /** Deactivated (issue #654): offered nowhere, only names their old messages. */
  @SerialName("is_deactivated") val isDeactivated: Boolean = false,
  /** Deleted (issue #630): a tombstone the app names by its own "Deleted user" (issue #751). */
  @SerialName("is_deleted") val isDeleted: Boolean = false,
) {
  /** Whether lists, pickers and mentions may offer this person. */
  val isOffered: Boolean get() = !isDeactivated && !isDeleted

  /** Named [deletedUserName] and without a username if the account was deleted, as on desktop and web. */
  fun namedIfDeleted(deletedUserName: String): User =
    if (isDeleted) copy(displayName = deletedUserName, username = "") else this
}

@Serializable
data class Space(
  val id: String,
  val name: String,
  @SerialName("unread_count") val unreadCount: Int = 0,
  @SerialName("mention_count") val mentionCount: Int = 0,
  /** Whether the user may create channels here, as the core decided it (issue #758). */
  @SerialName("can_create_channels") val canCreateChannels: Boolean = true,
  /** Whether the user may add or invite members here, as the server said (#1337). */
  @SerialName("can_add_members") val canAddMembers: Boolean = false,
  /** Whether the user may invite guests here, as the server said (#1337). */
  @SerialName("can_invite_guests") val canInviteGuests: Boolean = false,
  /** Version of the space's picture; without one the space shows no picture. */
  @SerialName("maybe_image_version") val maybeImageVersion: String? = null,
  /**
   * The DMs and group DMs whose other people all belong to the space (#701), as the core decided
   * it from the member list this device holds. They stay on Home too.
   */
  @SerialName("direct_conversations") val directConversations: List<String> = emptyList(),
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
  @SerialName("is_archived") val isArchived: Boolean = false,
  val bookmarks: List<Bookmark> = emptyList(),
  /** The user's notes to themself (issue #829); their `name` is empty. Absent from older cores. */
  @SerialName("is_notes_to_self") val isNotesToSelf: Boolean = false,
  /** Still being caught up after the device was away (issue #247): newer messages may come. */
  @SerialName("is_catching_up") val isCatchingUp: Boolean = false,
  /** The channel permissions the server says the user holds here; `null` when it didn't say. */
  @SerialName("maybe_permissions") val maybePermissions: List<String>? = null,
) {
  val isDirect: Boolean get() = kind == "Direct" || kind == "Group"

  /** "Add people" shows: the server says the user may manage the channel's members (#1534). */
  val canAddMembers: Boolean get() = !isDirect && maybePermissions.orEmpty().contains("manage_members")
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
data class MessageSource(
  val name: String,
  @SerialName("picture_sha256") val maybePictureSha256: String? = null,
)

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
  /**
   * Set while this device can't read the message (issue #888): `"waiting"` (its key may still
   * come), `"not_available"` (none is expected here) or `"failed"` (it didn't decrypt). [body] is
   * empty then; the core replaces the row once it is readable.
   */
  @SerialName("placeholder") val maybePlaceholder: String? = null,
  /**
   * Posted by an integration source: shown with this name, marked as a bot, and picture instead of
   * the account's (docs/bridge.md, "Source identity"). The core sets it on a bot's message only.
   */
  @SerialName("source") val maybeSource: MessageSource? = null,
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
  /** The key backup as this device knows it (issue #410); absent before the first key bag sync. */
  val backup: KeyBackup? = null,
  /** Voice messages under the verified workspace policy (#204); absent: on, up to 10 minutes. */
  @SerialName("voice_message_policy") val voiceMessagePolicy: VoiceMessagePolicy = VoiceMessagePolicy(),
)

/** Whether voice messages may be recorded and how long (#204). Not a security boundary. */
@Serializable
data class VoiceMessagePolicy(
  val enabled: Boolean = true,
  @SerialName("max_minutes") val maxMinutes: Int = DEFAULT_MINUTES,
) {
  /** The longest recording, with a stored value outside 1..60 minutes clamped. */
  val maxSeconds: Int get() = maxMinutes.coerceIn(1, MAX_MINUTES) * 60

  companion object {
    const val DEFAULT_MINUTES = 10
    const val MAX_MINUTES = 60
  }
}

/** Facts about the key backup; the settings put them into words. */
@Serializable
data class KeyBackup(
  /** The encrypted key bag is on the server. */
  @SerialName("is_stored") val isStored: Boolean = false,
  @SerialName("has_pending_upload") val hasPendingUpload: Boolean = false,
  /** `passphrase`, `passkey`, `organization`; unknown values are skipped. */
  val recovery: List<String> = emptyList(),
  /** When the recovery passphrase was last set (epoch milliseconds), if a device recorded it. */
  @SerialName("passphrase_set_at") val passphraseSetAt: Long? = null,
) {
  val hasPassphrase: Boolean get() = "passphrase" in recovery
  val hasOrganization: Boolean get() = "organization" in recovery
}

@Serializable
data class ContactKeys(
  val user: String,
  val fingerprint: String,
  @SerialName("maybe_new_fingerprint") val maybeNewFingerprint: String? = null,
)

@Serializable
data class ChatSnapshot(
  val me: User,
  /** The people to offer in lists, pickers and mentions. */
  val users: List<User> = emptyList(),
  val spaces: List<Space> = emptyList(),
  val conversations: List<Conversation> = emptyList(),
  val security: SecuritySnapshot = SecuritySnapshot(),
  /** Deactivated and deleted accounts: offered nowhere, they only name their old messages. */
  @SerialName("former_users") val formerUsers: List<User> = emptyList(),
) {
  /** Anyone this device knows, former accounts included (the authors of old messages). */
  fun user(id: String): User? =
    users.firstOrNull { it.id == id } ?: me.takeIf { it.id == id } ?: formerUsers.firstOrNull { it.id == id }

  fun conversation(id: String): Conversation? = conversations.firstOrNull { it.id == id }
  fun name(userId: String): String = user(userId)?.displayName ?: userId

  /**
   * Deleted accounts named [deletedUserName], the app's own "Deleted user" in the user's language,
   * instead of the server's placeholder (issue #751).
   */
  fun namingDeletedUsers(deletedUserName: String): ChatSnapshot =
    copy(
      users = users.map { it.namedIfDeleted(deletedUserName) },
      formerUsers = formerUsers.map { it.namedIfDeleted(deletedUserName) },
    )
}

@Serializable
data class MessageWindow(
  val conversation: String,
  val start: Int,
  val total: Int,
  val messages: List<Message>,
  /** Received but not readable on this device yet (keys not arrived); placeholders in [messages]. */
  val undecrypted: Int = 0,
)

/** The newest messages of a conversation down to the oldest one a feed shows, and the page before it. */
@Serializable
data class MessageTail(
  val conversation: String,
  val messages: List<Message>,
  /** Older messages exist: the feed can page further back. */
  @SerialName("has_older") val hasOlder: Boolean,
  /** Received but not readable on this device yet (keys not arrived); placeholders in [messages]. */
  val undecrypted: Int = 0,
  /** History this device never read between [messages], oldest first (issues #1156, #1438). */
  val gaps: List<HistoryGap> = emptyList(),
)

/**
 * A hole in the history among a [MessageTail]'s messages: a catch-up after a long absence read a
 * busy conversation's newest page ahead of the rest, and reads the rest by itself; a reader who
 * comes near asks for a page of it ([ChatActions.fillHistoryGap]).
 */
@Serializable
data class HistoryGap(
  /** The newest message before it: what a request names it by. */
  val after: String,
  /** How many of the tail's messages come before it (0: above the first). */
  val index: Int,
  /**
   * How the conversation's last request for a hole stands, as [de.zeitgleich.teamcraft.ui.ServerHistory]
   * says it (never `complete`).
   */
  val status: String,
)

@Serializable
data class ReplyPreview(
  @SerialName("maybe_message") val maybeMessage: String? = null,
  val author: String,
  val excerpt: String,
  /** The one picture of an album the reply answers (#241); it still shows when the picture is gone. */
  @SerialName("attachment") val maybeAttachment: ReplyAttachmentPreview? = null,
)

/** The picture a reply quotes. */
@Serializable
data class ReplyAttachmentPreview(
  @SerialName("file_id") val fileId: String,
  @SerialName("file_name") val fileName: String,
  @SerialName("thumbnail_png") val maybeThumbnail: ImageBytes? = null,
  /** The picture is still in the quoted message: the quote can open it. */
  @SerialName("can_open") val canOpen: Boolean = false,
)

@Serializable
data class ForwardedFrom(
  @SerialName("author_name") val authorName: String,
  @SerialName("maybe_conversation_name") val maybeConversationName: String? = null,
  @SerialName("sent_at") val sentAt: Long,
)

@Serializable
data class MessageEdit(val body: String, @SerialName("replaced_at") val replacedAt: Long)

/**
 * Where a file stands: `remote`, `uploading`, `downloading`, `local`, `failed`, or for an own file
 * that stopped going up `upload_stalled` (retried on "Try again") and `upload_failed` (for good).
 */
@Serializable
data class FileTransfer(
  val state: String,
  @SerialName("done_bytes") val doneBytes: Long = 0,
  @SerialName("total_bytes") val totalBytes: Long = 0,
  /** Failed attempts of an upload that is retried by itself. */
  @SerialName("failed_attempts") val failedAttempts: Int = 0,
  val path: String? = null,
  val reason: String? = null,
) {
  val isMoving: Boolean get() = state == "uploading" || state == "downloading"

  /** An own file still on its way up, or stopped: it can be cancelled. */
  val isCancellable: Boolean get() = state == "uploading" || state == "upload_stalled"

  /** An upload whose attempts failed so far and that the core retries by itself (#742). */
  val isRetrying: Boolean get() = state == "uploading" && doneBytes == 0L && failedAttempts > 0

  /** It stopped because the file storage (or the server) is down, not this device's network. */
  val isStorageUnavailable: Boolean get() = reason == "storage_unavailable"

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
  /** The sender's preview of an image (PNG or JPEG; decoders read the format from the bytes). */
  @SerialName("maybe_thumbnail_png") val maybeThumbnail: ImageBytes? = null,
  val transfer: FileTransfer,
  /**
   * Who wrote the message and when (Unix milliseconds), for the image viewer: the original author
   * and time of an imported message (e2ee.md §4.10).
   */
  @SerialName("maybe_sender") val maybeSender: String? = null,
  @SerialName("maybe_sent_at") val maybeSentAt: Long? = null,
  /** Who wrote an imported message when that is nobody with an account here. */
  @SerialName("imported_author") val maybeImportedAuthor: ImportedAuthor? = null,
  /** How long a video or voice message plays, from the sender. */
  @SerialName("maybe_duration_ms") val maybeDurationMs: Long? = null,
  /** A voice message's waveform from the sender (#204), one loudness byte (0-255) per bar; checked by the core. */
  @SerialName("maybe_waveform") val maybeWaveform: List<Int>? = null,
) {
  val isImage: Boolean get() = mediaType.startsWith("image/")

  /** A voice message (#204): audio with the sender's waveform, on the server or here, not still going up. */
  val isVoice: Boolean
    get() = mediaType.startsWith("audio/") && !maybeWaveform.isNullOrEmpty() && transfer.state in setOf("local", "remote", "downloading", "failed")
  val isVideo: Boolean get() = mediaType.startsWith("video/")

  /** A video that opens in the player: on the server or here, not one still going up. */
  val opensInVideoPlayer: Boolean get() = isVideo && transfer.state in setOf("local", "remote", "downloading", "failed")
}

/** Who wrote an imported message: `person` (the file's sender), `deleted` or `named` (a bot). */
@Serializable
data class ImportedAuthor(val kind: String, val name: String? = null) {
  /** The name shown instead of an account: a bot's old name, or [deletedName]; null for a person. */
  fun standInName(deletedName: String): String? =
    when (kind) {
      "named" -> name
      "deleted" -> deletedName
      else -> null
    }
}

@Serializable
data class LinkPreview(
  val url: String,
  @SerialName("maybe_title") val maybeTitle: String? = null,
  @SerialName("maybe_description") val maybeDescription: String? = null,
  @SerialName("maybe_site_name") val maybeSiteName: String? = null,
  @SerialName("maybe_image") val maybeImage: ImageBytes? = null,
)

/**
 * Picture bytes, which the bridge sends as a JSON array of numbers. Decoded straight into a byte
 * array: a `List<Int>` boxed every byte of every thumbnail on each read of the feed (#315). Equal by
 * content, so an unchanged message doesn't recompose.
 */
@Serializable(with = ImageBytesSerializer::class)
class ImageBytes(val bytes: ByteArray) {
  private val hash by lazy { bytes.contentHashCode() }

  override fun equals(other: Any?): Boolean = other is ImageBytes && bytes.contentEquals(other.bytes)

  override fun hashCode(): Int = hash
}

@OptIn(ExperimentalSerializationApi::class)
object ImageBytesSerializer : KSerializer<ImageBytes> {
  private val numbers = IntArraySerializer()
  override val descriptor: SerialDescriptor = SerialDescriptor("de.zeitgleich.teamcraft.ImageBytes", numbers.descriptor)

  override fun deserialize(decoder: Decoder): ImageBytes {
    val values = numbers.deserialize(decoder)
    return ImageBytes(ByteArray(values.size) { values[it].toByte() })
  }

  override fun serialize(encoder: Encoder, value: ImageBytes) {
    numbers.serialize(encoder, IntArray(value.bytes.size) { value.bytes[it].toInt() and 0xFF })
  }
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
  /** The server rejected this own message; it waits for "retry" or "delete". */
  @SerialName("is_send_failed") val isSendFailed: Boolean = false,
  /** An own message the other member of a DM read: two ticks instead of one (issue #769). */
  @SerialName("is_read") val isRead: Boolean = false,
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
  @SerialName("is_notes_to_self") val isNotesToSelf: Boolean = false,
  @SerialName("saved_at") val savedAt: Long,
)

/** One unsent text, in a conversation or one of its threads (#1220). */
@Serializable
data class DraftItem(
  val conversation: String,
  @SerialName("maybe_thread") val maybeThread: String? = null,
  @SerialName("conversation_name") val conversationName: String,
  @SerialName("is_notes_to_self") val isNotesToSelf: Boolean = false,
  @SerialName("maybe_thread_root") val maybeThreadRoot: String? = null,
  val body: String,
  @SerialName("edited_at") val editedAt: Long,
  /** Files attached to it; a draft may hold files and no text. */
  @SerialName("file_count") val fileCount: Int = 0,
)

/** A file attached to a draft (#1220), as the composer shows it. */
@Serializable
data class DraftFileItem(
  /** Names it for `FileSource::Draft` and for removing it. */
  val id: String,
  @SerialName("file_name") val fileName: String,
  @SerialName("media_type") val mediaType: String,
  @SerialName("size_bytes") val sizeBytes: Long = 0,
  @SerialName("thumbnail") val maybeThumbnail: ImageBytes? = null,
  @SerialName("state") val wireState: DraftFileStateWire = DraftFileStateWire(),
) {
  val state: DraftFileState get() = DraftFileState.of(wireState.state)

  /** Goes out with the message: not one that failed or expired. */
  val isSendable: Boolean get() = state == DraftFileState.UPLOADING || state == DraftFileState.READY || state == DraftFileState.ON_THIS_DEVICE
}

/** `DraftFileState`, tagged by `state` (`{"state":"ready"}`). */
@Serializable
data class DraftFileStateWire(val state: String = "unknown")

/** Where a draft's file stands; one this app doesn't know counts as failed. */
enum class DraftFileState(val wire: String) {
  UPLOADING("uploading"),
  READY("ready"),
  ON_THIS_DEVICE("on_this_device"),
  FAILED("failed"),
  EXPIRED("expired"),
  UNKNOWN("unknown"),
  ;

  companion object {
    fun of(wire: String): DraftFileState = entries.firstOrNull { it.wire == wire } ?: UNKNOWN
  }
}

/** The answer of the `draft_files` query. */
@Serializable
data class DraftFilesAnswer(val files: List<DraftFileItem> = emptyList())

/** One message the user sent, as much of it as the list shows (#1220). */
@Serializable
data class SentItem(
  val conversation: String,
  @SerialName("conversation_name") val conversationName: String,
  @SerialName("is_notes_to_self") val isNotesToSelf: Boolean = false,
  val message: String,
  @SerialName("maybe_thread") val maybeThread: String? = null,
  val excerpt: String = "",
  @SerialName("file_count") val fileCount: Int = 0,
  @SerialName("sent_at") val sentAt: Long,
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
  /** How the user gets in (`open`, `as_admin`, `needs_approval`, `requested`); see `ui.BrowseJoin`. */
  val join: String = "open",
)

/** A guest membership ending soon (`expiring_guests`), issue #212. */
@Serializable
data class ExpiringGuest(
  @SerialName("space_id") val spaceId: String,
  @SerialName("space_name") val spaceName: String,
  val user: ExpiringGuestUser,
  @SerialName("expires_at") val expiresAt: Long,
  @SerialName("is_own") val isOwn: Boolean = false,
  @SerialName("extend_offers") val extendOffers: List<GuestExtendOffer> = emptyList(),
)

/** What the user may extend to, as the server decided it; without `days`, as far as their limit goes. */
@Serializable
data class GuestExtendOffer(
  val until: Long,
  val days: Int? = null,
)

@Serializable
data class ExpiringGuestUser(
  val id: String,
  @SerialName("display_name") val displayName: String,
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
  @SerialName("is_notes_to_self") val isNotesToSelf: Boolean = false,
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
  /** [ChatQueries.draftReply]: what the composer replies to. */
  @SerialName("maybe_reply") val maybeReply: ReplyPreview? = null,
  val drafts: List<DraftItem> = emptyList(),
  val sent: List<SentItem> = emptyList(),
  val files: List<FileInfo> = emptyList(),
  @SerialName("maybe_bytes") val maybeBytes: Long? = null,
  val channels: List<BrowsableChannel> = emptyList(),
  val guests: List<ExpiringGuest> = emptyList(),
  val hits: List<SearchHit> = emptyList(),
  val results: SearchResults? = null,
  val value: Boolean = false,
  val threads: List<ThreadSummary> = emptyList(),
  val users: List<String> = emptyList(),
  /** [ChatQueries.typingNow]: everyone typing anywhere right now. */
  val typing: List<TypingPlace> = emptyList(),
  val unseen: UnseenReactions? = null,
  @SerialName("older_history") val olderHistory: String? = null,
  val calls: List<CallHistoryItem> = emptyList(),
  val overview: ScheduledOverview? = null,
  val load: ListLoadWire? = null,
  /** [ChatQueries.joinableSpaces]: open spaces the user isn't in (#1526). */
  val spaces: List<JoinableSpace> = emptyList(),
  /** [ChatClient.searchPeople]: people by name, the server's directory included (#1534). */
  val people: List<User> = emptyList(),
)

/** An open space the user may join, as "Browse spaces" lists it (#1526). */
@Serializable
data class JoinableSpace(
  val id: String,
  val name: String,
  @SerialName("maybe_description") val maybeDescription: String? = null,
  @SerialName("maybe_image_version") val maybeImageVersion: String? = null,
  @SerialName("member_count") val memberCount: Int = 0,
)

/** Others typing in one place right now (#882): a conversation itself, or with a thread a reply in it. */
@Serializable
data class TypingPlace(
  val conversation: String,
  @SerialName("maybe_thread") val maybeThread: String? = null,
  val users: List<String> = emptyList(),
)

/**
 * Others' reactions to the user's own messages in a conversation that the user hasn't seen: the
 * heart button over the feed (notifications.md, "Reactions to my messages").
 */
@Serializable
data class UnseenReactions(
  val count: Int = 0,
  /** The messages they react to, oldest first. */
  val messages: List<String> = emptyList(),
)

/** Global notification preferences the server enforces (`/me/notification-settings`). */
@Serializable
data class NotificationSettings(
  @SerialName("auto_follow_mentioned_threads") val autoFollowMentionedThreads: Boolean = true,
  @SerialName("push_mode") val pushMode: String = "away_only",
  @SerialName("send_read_receipts") val sendReadReceipts: Boolean = true,
  @SerialName("dnd_schedule") val maybeDndSchedule: DndSchedule? = null,
  /** Minutes before a meeting to remind (0: not at all); absent: never chose (5). */
  @SerialName("meeting_reminder_minutes") val maybeMeetingReminderMinutes: Int? = null,
  /** Whether a meeting's start notifies; absent: never chose (yes). */
  @SerialName("notifies_meeting_start") val maybeNotifiesMeetingStart: Boolean? = null,
)

/**
 * Per-conversation level (`all`, `mentions`, `none`), mute time and filters (notifications.md,
 * "Per-conversation filters"). Filters left `null` keep what the server has.
 */
@Serializable
data class ConversationNotificationSettings(
  val level: String = "all",
  @SerialName("muted_until") val mutedUntil: Long? = null,
  /** Follows the space's setting; saving a level or mute makes it the conversation's own. */
  @SerialName("is_inherited") val isInherited: Boolean = false,
  /** `default`, `notify` or `ignore`: whether `@channel` and `@here` notify here; absent reads as `default`. */
  @SerialName("channel_mentions") val channelMentions: String? = null,
  /** Times the conversation stays silent as if muted; no windows removes them. */
  @SerialName("quiet_hours") val quietHours: QuietHours? = null,
)

/** Quiet hours of one conversation, in the user's zone like the DND schedule. */
@Serializable
data class QuietHours(
  @SerialName("utc_offset_minutes") val utcOffsetMinutes: Int = 0,
  @SerialName("time_zone") val maybeTimeZone: String? = null,
  val windows: List<DndWindow> = emptyList(),
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
  /** The space it was scheduled in; its chat stays outside spaces. */
  @SerialName("space_id") val maybeSpaceId: String? = null,
)

@Serializable
data class MeetingList(val meetings: List<Meeting> = emptyList())

/**
 * `GET /me/holiday-region` and the answer of `PUT` (#1450): whose public holidays the member's
 * repeating meetings skip. [maybeRegion] is absent while the workspace's region applies.
 */
@Serializable
data class HolidayRegion(
  @SerialName("region") val maybeRegion: String? = null,
  /** The workspace's region, which applies without an own one; empty: none. */
  @SerialName("instance_region") val instanceRegion: String = "",
  /** The workspace has a holiday calendar whose holidays are skipped as well. */
  @SerialName("has_instance_feed") val hasInstanceFeed: Boolean = false,
)

/** `PUT /me/holiday-region`: no region (or an empty one) goes back to the workspace's. */
@Serializable
data class SetHolidayRegion(@SerialName("region") val maybeRegion: String? = null)

/** The regions of the server's holiday table (`HOLIDAY_REGIONS`), in its order. */
val HOLIDAY_REGIONS = listOf(
  "DE", "DE-BW", "DE-BY", "DE-BE", "DE-BB", "DE-HB", "DE-HH", "DE-HE", "DE-MV", "DE-NI", "DE-NW", "DE-RP",
  "DE-SL", "DE-SN", "DE-ST", "DE-SH", "DE-TH", "AT", "CH", "CH-AG", "CH-AI", "CH-AR", "CH-BE", "CH-BL",
  "CH-BS", "CH-FR", "CH-GE", "CH-GL", "CH-GR", "CH-JU", "CH-LU", "CH-NE", "CH-NW", "CH-OW", "CH-SG", "CH-SH",
  "CH-SO", "CH-SZ", "CH-TG", "CH-TI", "CH-UR", "CH-VD", "CH-VS", "CH-ZG", "CH-ZH",
)

/** Reads one string field of a JSON object answer, for small replies. */
fun JsonElement.stringField(name: String): String? = (jsonObject[name] as? JsonPrimitive)?.content

/**
 * The member's own profile (`/me/profile`), kept as the server sent it: the update replaces the
 * whole profile, so a change to the name sends the other fields back as they are ([updateBody]).
 */
class OwnProfile(private val raw: JsonObject) {
  /** The workspace policy lets people replace the name from the identity provider. */
  val canOverrideDisplayName: Boolean
    get() = (raw["can_override_display_name"] as? JsonPrimitive)?.content == "true"

  /** The name the member chose, `null` while the sign-in name is shown. */
  val maybeDisplayNameOverride: String?
    get() = (raw["display_name_override"] as? JsonPrimitive)?.takeIf { it.isString }?.content

  /** The body of `PUT /me/profile` with [maybeName] as the name (`null`: the sign-in name). */
  fun updateBody(maybeName: String?): String {
    val kept = raw.filterKeys { it in PROFILE_FIELDS }
    val fields = maybeName?.let { kept + ("display_name" to JsonPrimitive(it)) } ?: kept
    return JsonObject(fields).toString()
  }

  companion object {
    private val PROFILE_FIELDS = setOf("title", "department", "phone", "time_zone", "working_hours")

    fun parse(json: String): OwnProfile = OwnProfile(WireJson.parseToJsonElement(json).jsonObject)
  }
}

/** Why a list could not be read (`LoadFailure` of the core); a cause of a newer core reads as [OTHER]. */
@Serializable(with = LoadFailureSerializer::class)
enum class LoadFailure(val wire: String) {
  NETWORK("network"),
  SERVER("server"),
  TIMED_OUT("timed_out"),
  OTHER("other"),
}

object LoadFailureSerializer : kotlinx.serialization.KSerializer<LoadFailure> {
  override val descriptor = kotlinx.serialization.descriptors.PrimitiveSerialDescriptor(
    "LoadFailure",
    kotlinx.serialization.descriptors.PrimitiveKind.STRING,
  )

  override fun deserialize(decoder: kotlinx.serialization.encoding.Decoder): LoadFailure {
    val wire = decoder.decodeString()
    return LoadFailure.entries.firstOrNull { it.wire == wire } ?: LoadFailure.OTHER
  }

  override fun serialize(encoder: kotlinx.serialization.encoding.Encoder, value: LoadFailure) =
    encoder.encodeString(value.wire)
}

/** How far a list is, as the core knows it (`ListLoad`, tagged by `state`). */
@Serializable
data class ListLoadWire(
  val state: String = "loading",
  val failure: LoadFailure = LoadFailure.OTHER,
) {
  /** An unknown state of a newer core is not waited for. */
  fun toLoad(): ListLoad = when (state) {
    "loading" -> ListLoad.Loading
    "failed" -> ListLoad.Failed(failure)
    else -> ListLoad.Ready
  }
}

sealed interface ListLoad {
  data object Loading : ListLoad

  data object Ready : ListLoad

  data class Failed(val failure: LoadFailure) : ListLoad
}

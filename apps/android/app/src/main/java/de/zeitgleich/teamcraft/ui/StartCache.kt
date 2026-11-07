package de.zeitgleich.teamcraft.ui

import java.io.File
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import de.zeitgleich.teamcraft.security.KeystoreBox
import de.zeitgleich.teamcraft.security.SealedCache
import de.zeitgleich.teamcraft.security.SealedFile
import uniffi.teamcraft_ffi.ConversationItem
import uniffi.teamcraft_ffi.ConversationKind
import uniffi.teamcraft_ffi.PeerInfo
import uniffi.teamcraft_ffi.Presence

/**
 * What the app showed last: the account and its chat list, so the next start shows them in its first
 * frame instead of a loading screen while the store opens (about half a second on an older phone).
 * The real state replaces it as soon as it is read. Sealed like the database key (e2ee.md §9.2) and
 * deleted at sign-out.
 */
@Serializable
data class StartCache(
  /** The server's origin and the account's subject, as the store is keyed. */
  val account: String,
  val userId: String,
  val displayName: String,
  val identityFingerprint: String,
  val conversations: List<CachedConversation>,
  val previews: Map<String, CachedEntry>,
) {
  fun ready(): AppState.Ready = AppState.Ready(userId, displayName, identityFingerprint)

  /** The list as it was, without running calls: those are never stale on screen. */
  fun listState(): ChatListState = ChatListState(
    maybeConversations = conversations.mapNotNull(CachedConversation::item),
    previews = previews.mapValues { (_, entry) -> entry.entry() },
  )

  companion object {
    /** `null` until the list was read once: there is nothing worth showing yet. */
    fun of(account: String, ready: AppState.Ready, list: ChatListState): StartCache? {
      val conversations = list.maybeConversations ?: return null
      return StartCache(
        account = account,
        userId = ready.userId,
        displayName = ready.displayName,
        identityFingerprint = ready.identityFingerprint,
        conversations = conversations.map(CachedConversation::of),
        previews = list.previews.mapValues { (_, entry) -> CachedEntry.of(entry) },
      )
    }
  }
}

/** The fields of a [ConversationItem] the lists use; what is left out takes its default. */
@Serializable
data class CachedConversation(
  val id: String,
  val maybeSpaceId: String? = null,
  val maybeSpaceName: String? = null,
  val kind: String,
  val name: String,
  val topic: String = "",
  val unreadCount: Int = 0,
  val mentionCount: Int = 0,
  val isFavorite: Boolean = false,
  val isMuted: Boolean = false,
  val canPost: Boolean = true,
  val maybePeerPresence: String? = null,
  val isArchived: Boolean = false,
  val maybePeerId: String? = null,
  val maybePeerIsBot: Boolean? = null,
  val maybePeerHue: Int? = null,
  val isNotesToSelf: Boolean = false,
  val maybeDraftPreview: String? = null,
  val maybeSpaceImageVersion: String? = null,
) {
  /** `null` for a kind this version doesn't know: a cache written by a newer one. */
  fun item(): ConversationItem? {
    val maybeKind = ConversationKind.entries.firstOrNull { it.name == kind } ?: return null
    return ConversationItem(
      id = id,
      maybeSpaceId = maybeSpaceId,
      maybeSpaceName = maybeSpaceName,
      kind = maybeKind,
      name = name,
      topic = topic,
      unreadCount = unreadCount.toUInt(),
      mentionCount = mentionCount.toUInt(),
      isFavorite = isFavorite,
      isMuted = isMuted,
      canPost = canPost,
      maybePeerPresence = Presence.entries.firstOrNull { it.name == maybePeerPresence },
      isArchived = isArchived,
      maybePeerId = maybePeerId,
      maybePeer = if (maybePeerId != null && maybePeerIsBot != null && maybePeerHue != null) {
        PeerInfo(userId = maybePeerId, isBot = maybePeerIsBot, avatarHue = maybePeerHue.toUShort())
      } else {
        null
      },
      isNotesToSelf = isNotesToSelf,
      maybeDraftPreview = maybeDraftPreview,
      maybeSpaceImageVersion = maybeSpaceImageVersion,
    )
  }

  companion object {
    fun of(item: ConversationItem) = CachedConversation(
      id = item.id,
      maybeSpaceId = item.maybeSpaceId,
      maybeSpaceName = item.maybeSpaceName,
      kind = item.kind.name,
      name = item.name,
      topic = item.topic,
      unreadCount = item.unreadCount.toInt(),
      mentionCount = item.mentionCount.toInt(),
      isFavorite = item.isFavorite,
      isMuted = item.isMuted,
      canPost = item.canPost,
      maybePeerPresence = item.maybePeerPresence?.name,
      isArchived = item.isArchived,
      maybePeerId = item.maybePeerId,
      maybePeerIsBot = item.maybePeer?.isBot,
      maybePeerHue = item.maybePeer?.avatarHue?.toInt(),
      isNotesToSelf = item.isNotesToSelf,
      maybeDraftPreview = item.maybeDraftPreview,
      maybeSpaceImageVersion = item.maybeSpaceImageVersion,
    )
  }
}

/** A [ChatListEntry]: who wrote the newest message, what it says, when. */
@Serializable
data class CachedEntry(
  val sender: Sender,
  val maybeSenderName: String? = null,
  val content: Content,
  val maybeText: String? = null,
  val sentAt: Long,
  val lastMessageId: String,
) {
  @Serializable
  enum class Sender { NONE, ME, OTHER }

  @Serializable
  enum class Content { TEXT, DELETED, EVENT, PLACEHOLDER }

  fun entry(): ChatListEntry {
    val previewSender = when (sender) {
      Sender.NONE -> PreviewSender.None
      Sender.ME -> PreviewSender.Me
      Sender.OTHER -> PreviewSender.Other(maybeSenderName.orEmpty())
    }
    val previewContent = when (content) {
      Content.TEXT -> PreviewContent.Text(maybeText.orEmpty())
      Content.DELETED -> PreviewContent.Deleted
      Content.EVENT -> PreviewContent.Event
      Content.PLACEHOLDER -> PreviewContent.Placeholder(maybeText.orEmpty())
    }
    return ChatListEntry(ChatPreview(previewSender, previewContent, sentAt), lastMessageId)
  }

  companion object {
    fun of(entry: ChatListEntry): CachedEntry {
      val preview = entry.preview
      val (sender, maybeName) = when (val previewSender = preview.sender) {
        PreviewSender.None -> Sender.NONE to null
        PreviewSender.Me -> Sender.ME to null
        is PreviewSender.Other -> Sender.OTHER to previewSender.name
      }
      val (content, maybeText) = when (val previewContent = preview.content) {
        is PreviewContent.Text -> Content.TEXT to previewContent.text
        PreviewContent.Deleted -> Content.DELETED to null
        PreviewContent.Event -> Content.EVENT to null
        is PreviewContent.Placeholder -> Content.PLACEHOLDER to previewContent.state
      }
      return CachedEntry(sender, maybeName, content, maybeText, preview.sentAt, entry.lastMessageId)
    }
  }
}

/**
 * Where [StartCache] lives: one per device, the account's own is checked by [StartCache.account].
 * One instance for the app (`AppGraph.startCache`); every access holds its lock, so a sign-out's
 * delete and a write never cross.
 */
class StartCacheStore(filesDir: File) {
  private val sealed = SealedCache(
    File(filesDir, FILE_NAME),
    SealedFile(File(filesDir, KEY_FILE_NAME), KeystoreBox(KEY_ALIAS)),
  )

  /** `null` when there is none, or it can't be read (written by another version, or damaged). */
  @Synchronized
  fun read(): StartCache? = sealed.read()?.let { bytes ->
    runCatching { json.decodeFromString<StartCache>(bytes.decodeToString()) }.getOrNull()
  }

  /** Writes [cache] if [isStillWanted] holds, checked under the lock (the session may just have ended). */
  @Synchronized
  fun write(cache: StartCache, isStillWanted: () -> Boolean) {
    if (isStillWanted()) sealed.write(json.encodeToString(cache).encodeToByteArray())
  }

  @Synchronized
  fun delete() = sealed.delete()

  private companion object {
    const val FILE_NAME = "start-cache.sealed"
    const val KEY_FILE_NAME = "start-cache.key"
    const val KEY_ALIAS = "teamcraft_start_cache"
    val json = Json { ignoreUnknownKeys = true }
  }
}

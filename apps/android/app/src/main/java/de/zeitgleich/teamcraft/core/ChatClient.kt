package de.zeitgleich.teamcraft.core

import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.longOrNull
import uniffi.teamcraft_ffi.LeaveChoice
import uniffi.teamcraft_ffi.SpaceMenuItem
import uniffi.teamcraft_ffi.TeamCraft

/**
 * The chat features of the core, decoded: reads go through the JSON bridge's snapshot, window and
 * queries, changes through actions ([ChatActions]). Nothing here interprets the data; only the
 * snapshot's deleted accounts get [deletedUserName] when one is given (issue #751).
 */
class ChatClient(
  /** The app's "Deleted user" in the user's language; `null` for readers that show no old messages. */
  private val deletedUserName: (() -> String)? = null,
  /**
   * The store's change counter ([CoreRepository.changeCount]) and the account's generation
   * ([CoreRepository.accountGeneration]), when the reader has them: the snapshot is then read and
   * decoded once per change and shared by every screen that asks, never across accounts.
   */
  private val maybeChanges: (() -> Long)? = null,
  private val generation: () -> Long = { 0L },
  private val core: suspend () -> TeamCraft,
) {
  private val snapshotLock = Mutex()
  private val kept = KeptValue<ChatSnapshot>()

  /**
   * The whole workspace (users, spaces, conversations). Decoding it takes hundreds of milliseconds
   * on a slow phone with a large workspace, so it is kept until the next change.
   */
  suspend fun snapshot(): ChatSnapshot {
    val changes = maybeChanges ?: return readSnapshot()
    return snapshotLock.withLock {
      // Read before the snapshot: a change while it is read makes the next caller read again.
      val account = generation()
      val version = changes()
      kept.at(account, version) ?: readSnapshot().also { kept.keep(account, version, it) }
    }
  }

  /**
   * The snapshot read last, maybe from before the newest change, or `null` before the first read: a
   * screen shows names and which messages are the user's own with it at once, then reads afresh.
   */
  fun maybeKeptSnapshot(): ChatSnapshot? = kept.latest(generation())

  /** Drops the kept snapshot: the session ended, its account's data leaves memory. */
  fun forget() = kept.clear()

  private suspend fun readSnapshot(): ChatSnapshot {
    val snapshot = WireJson.decodeFromString(ChatSnapshot.serializer(), core().snapshot())
    return deletedUserName?.let { snapshot.namingDeletedUsers(it()) } ?: snapshot
  }

  /** The newest `limit` messages of a conversation, oldest first. */
  suspend fun latest(conversation: String, limit: Int): MessageWindow {
    val probe = window(conversation, 0, 0)
    val start = (probe.total - limit).coerceAtLeast(0)
    return window(conversation, start, limit)
  }

  suspend fun window(conversation: String, start: Int, len: Int): MessageWindow =
    WireJson.decodeFromString(
      MessageWindow.serializer(),
      core().messageWindow(conversation, start.toUInt(), len.toUInt()),
    )

  /**
   * The messages of a conversation from [maybeOldest] (the oldest one a feed shows) to the newest,
   * and [older] messages before it; without [maybeOldest] the newest [older] (issue #1058).
   */
  suspend fun tail(conversation: String, maybeOldest: Message?, older: Int): MessageTail =
    WireJson.decodeFromString(
      MessageTail.serializer(),
      core().messageTail(conversation, maybeOldest?.id, maybeOldest?.sentAt ?: 0, older.toUInt()),
    )

  /** The newest [len] messages, read ahead without moving the catch-up focus (the user isn't there yet). */
  suspend fun tailAhead(conversation: String, len: Int): MessageTail =
    WireJson.decodeFromString(MessageTail.serializer(), core().messageTailAhead(conversation, len.toUInt()))

  suspend fun query(query: String): ChatAnswer = decodeAnswer(core().query(query))

  /** A query whose answer goes back to the core undecoded (the image viewer's pages). */
  suspend fun rawQuery(query: String): String = core().query(query)

  suspend fun act(action: String) = core().act(action)

  /** The files of a draft (#1220), in the order they were attached. */
  suspend fun draftFiles(conversation: String, maybeThread: String?): List<DraftFileItem> =
    decodeDraftFiles(core().query(ChatQueries.draftFiles(conversation, maybeThread)))

  suspend fun extras(conversation: String, messages: List<String>): Map<String, MessageExtras> =
    if (messages.isEmpty()) {
      emptyMap()
    } else {
      query(ChatQueries.messageExtras(conversation, messages)).extras.associateBy { it.message }
    }

  /** A plain server endpoint (notification settings, meetings): answers its JSON. */
  suspend fun api(method: String, path: String, maybeBody: String? = null): String =
    core().apiRequest(method, path, maybeBody)

  suspend fun ownProfile(): OwnProfile = OwnProfile.parse(api("GET", "/me/profile"))

  /** Saves [maybeName] as the member's name, or with `null` goes back to the sign-in name. */
  suspend fun setDisplayName(profile: OwnProfile, maybeName: String?) {
    api("PUT", "/me/profile", profile.updateBody(maybeName))
  }

  suspend fun notificationSettings(): NotificationSettings =
    WireJson.decodeFromString(NotificationSettings.serializer(), api("GET", "/me/notification-settings"))

  suspend fun setNotificationSettings(settings: NotificationSettings) {
    api("PUT", "/me/notification-settings", WireJson.encodeToString(NotificationSettings.serializer(), settings))
  }

  suspend fun conversationNotifications(conversation: String): ConversationNotificationSettings =
    WireJson.decodeFromString(
      ConversationNotificationSettings.serializer(),
      api("GET", "/conversations/$conversation/notification-settings"),
    )

  suspend fun setConversationNotifications(conversation: String, settings: ConversationNotificationSettings) {
    api(
      "PUT",
      "/conversations/$conversation/notification-settings",
      WireJson.encodeToString(ConversationNotificationSettings.serializer(), settings),
    )
  }

  /** The core's answer to whether the conversation's menu offers "Leave" (#1105). */
  suspend fun leaveChoice(conversation: String): LeaveChoice = core().conversationLeaveChoice(conversation)

  /** `{origin}/c/{id}`, or `null` where the server has no web address. */
  fun conversationLink(origin: String, conversation: String): String? =
    uniffi.teamcraft_ffi.conversationLink(origin, conversation)

  suspend fun spaceMenu(space: String): SpaceMenuItem? = core().spaceMenu(space)

  /** How many conversations the server just claimed deleted at once, if suspect; told once. */
  suspend fun takeSuspectDeletions(): UInt = core().takeSuspectDeletions()

  /** The space's mute time (`muted_until`), if it is muted ahead. */
  suspend fun spaceMutedUntil(space: String, nowMs: Long): Long? =
    spaceSettings(space)["muted_until"]?.jsonPrimitive?.longOrNull?.takeIf { it > nowMs }

  /** Mutes the space until `maybeUntil` (`null` unmutes); the space's level stays as it is. */
  suspend fun setSpaceMutedUntil(space: String, maybeUntil: Long?) {
    val body = JsonObject(
      spaceSettings(space).filterKeys { it != "muted_until" } +
        (if (maybeUntil == null) emptyMap() else mapOf("muted_until" to JsonPrimitive(maybeUntil))),
    )
    api("PUT", "/spaces/$space/notification-settings", body.toString())
  }

  private suspend fun spaceSettings(space: String): JsonObject =
    WireJson.parseToJsonElement(api("GET", "/spaces/$space/notification-settings")).jsonObject

  /** Quick DND (`manual = "dnd"`, until a time or open-ended) or back to automatic. */
  suspend fun setManualPresence(presence: ManualPresence) {
    api("PUT", "/me/presence", WireJson.encodeToString(ManualPresence.serializer(), presence))
  }

  /** What others see of `userId` now (`online`, `away`, `dnd`, `offline`); `null` if unknown. */
  suspend fun presenceState(userId: String): String? =
    WireJson.decodeFromString(PresenceList.serializer(), api("GET", "/presence?user_ids=$userId"))
      .presences.firstOrNull { it.userId == userId }?.state

  suspend fun meetings(): List<Meeting> =
    WireJson.decodeFromString(MeetingList.serializer(), api("GET", "/meetings")).meetings
}

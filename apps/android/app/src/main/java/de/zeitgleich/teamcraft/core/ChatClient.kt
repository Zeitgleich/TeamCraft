package de.zeitgleich.teamcraft.core

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
  private val core: suspend () -> TeamCraft,
) {
  suspend fun snapshot(): ChatSnapshot {
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

  suspend fun query(query: String): ChatAnswer = decodeAnswer(core().query(query))

  /** A query whose answer goes back to the core undecoded (the image viewer's pages). */
  suspend fun rawQuery(query: String): String = core().query(query)

  suspend fun act(action: String) = core().act(action)

  suspend fun extras(conversation: String, messages: List<String>): Map<String, MessageExtras> =
    if (messages.isEmpty()) {
      emptyMap()
    } else {
      query(ChatQueries.messageExtras(conversation, messages)).extras.associateBy { it.message }
    }

  /** A plain server endpoint (notification settings, meetings): answers its JSON. */
  suspend fun api(method: String, path: String, maybeBody: String? = null): String =
    core().apiRequest(method, path, maybeBody)

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

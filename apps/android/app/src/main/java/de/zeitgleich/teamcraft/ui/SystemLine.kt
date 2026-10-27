package de.zeitgleich.teamcraft.ui

import androidx.compose.runtime.Composable
import androidx.compose.ui.res.stringResource
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.Message
import de.zeitgleich.teamcraft.core.MessageExtras

/**
 * The line a system message shows instead of a bubble (issue #769), with the events and wording of
 * desktop and web. A change someone made to the conversation (the extras' workspace event) says
 * more than the plain join or leave the core reports with it: "Ada added Sam", not "Sam joined".
 */
sealed interface SystemLine {
  data class Joined(val name: String) : SystemLine

  data class Left(val name: String) : SystemLine

  data class Renamed(val name: String, val channel: String) : SystemLine

  data class TopicChanged(val name: String, val topic: String) : SystemLine

  data class MemberAdded(val name: String, val by: String, val sharesHistory: Boolean) : SystemLine

  data class MemberRemoved(val name: String, val by: String) : SystemLine

  data class Archived(val by: String) : SystemLine

  data class Unarchived(val by: String) : SystemLine

  data class MadePrivate(val by: String) : SystemLine

  data class MadePublic(val by: String) : SystemLine

  data class Pinned(val by: String) : SystemLine

  /** To the space [space]; empty if the mover's device didn't know its name. */
  data class Moved(val by: String, val space: String) : SystemLine

  /**
   * A workspace admin joined the private channel (admin.md §2.4): by themselves, or let in by the
   * channel owner [maybeApprovedBy] who answered their request.
   */
  data class AdminJoined(val name: String, val maybeApprovedBy: String?) : SystemLine

  /**
   * A workspace admin joined the private channel because [by] added them without an approval to
   * ask for: a member's ordinary add, or another admin's signed change (admin.md §2.4.2).
   */
  data class AdminAdded(val name: String, val by: String) : SystemLine

  /** A message without text or files of a kind this version doesn't know. */
  data object Changed : SystemLine
}

/**
 * The line [message] shows, or `null` for a message people wrote. [nameOf] names a user ID (empty
 * for someone this device doesn't know); the core makes the person an event is about its author.
 */
fun systemLine(message: Message, maybeExtras: MessageExtras?, nameOf: (String) -> String): SystemLine? =
  maybeExtras?.maybeWorkspaceEvent?.let { workspaceLine(it, nameOf) }
    ?: message.maybeSystemEvent?.let { systemEventLine(it, nameOf(message.author)) }
    ?: SystemLine.Changed.takeIf {
      message.body.isEmpty() && maybeExtras?.files.isNullOrEmpty() && maybeExtras?.isDeleted != true
    }

/** The line in the app's language. */
@Composable
fun SystemLine.localizedText(): String = when (this) {
  is SystemLine.Joined -> stringResource(R.string.event_joined, name)
  is SystemLine.Left -> stringResource(R.string.event_left, name)
  is SystemLine.Renamed -> stringResource(R.string.event_renamed, name, channel)
  is SystemLine.TopicChanged -> stringResource(R.string.event_topic, name, topic)
  is SystemLine.MemberAdded ->
    stringResource(if (sharesHistory) R.string.event_member_added_history else R.string.event_member_added, by, name)
  is SystemLine.MemberRemoved -> stringResource(R.string.event_member_removed, by, name)
  is SystemLine.Archived -> stringResource(R.string.event_archived, by)
  is SystemLine.Unarchived -> stringResource(R.string.event_unarchived, by)
  is SystemLine.MadePrivate -> stringResource(R.string.event_private, by)
  is SystemLine.MadePublic -> stringResource(R.string.event_public, by)
  is SystemLine.Pinned -> stringResource(R.string.event_pinned, by)
  is SystemLine.Moved -> if (space.isEmpty()) {
    stringResource(R.string.event_moved_unnamed, by)
  } else {
    stringResource(R.string.event_moved, by, space)
  }
  is SystemLine.AdminJoined -> maybeApprovedBy?.let { by ->
    stringResource(R.string.event_admin_joined_approved, name, by)
  } ?: stringResource(R.string.event_admin_joined, name)
  is SystemLine.AdminAdded -> stringResource(R.string.event_admin_joined_added, name, by)
  SystemLine.Changed -> stringResource(R.string.chat_preview_event)
}

/** The core's `SystemEvent`: `"Joined"`, `"Left"`, `{"Renamed":{"name":…}}`, `{"TopicChanged":{"topic":…}}`. */
private fun systemEventLine(event: JsonElement, name: String): SystemLine? {
  val (kind, maybeFields) = when (event) {
    is JsonPrimitive -> event.contentOrNull to null
    is JsonObject ->
      event.entries.singleOrNull()?.let { (key, value) -> key to (value as? JsonObject) } ?: (null to null)
    else -> null to null
  }
  return when (kind) {
    "Joined" -> SystemLine.Joined(name)
    "Left" -> SystemLine.Left(name)
    "Renamed" -> SystemLine.Renamed(name, maybeFields.text("name"))
    "TopicChanged" -> SystemLine.TopicChanged(name, maybeFields.text("topic"))
    else -> null
  }
}

/** The extras' `WorkspaceEvent`, tagged by `event`; `null` for one this version doesn't know. */
private fun workspaceLine(event: JsonObject, nameOf: (String) -> String): SystemLine? {
  val by = nameOf(event.text("by"))
  return when (event.text("event")) {
    "member_added" ->
      SystemLine.MemberAdded(nameOf(event.text("user")), by, sharesHistory = event.text("history") == "full")
    "member_removed" -> SystemLine.MemberRemoved(nameOf(event.text("user")), by)
    "archived" -> SystemLine.Archived(by)
    "unarchived" -> SystemLine.Unarchived(by)
    "made_private" -> SystemLine.MadePrivate(by)
    "made_public" -> SystemLine.MadePublic(by)
    "moved" -> SystemLine.Moved(by, event.text("space"))
    "pinned" -> SystemLine.Pinned(by)
    "admin_joined" -> event.text("added_by").takeIf { it.isNotEmpty() }?.let { addedBy ->
      SystemLine.AdminAdded(nameOf(event.text("user")), nameOf(addedBy))
    } ?: SystemLine.AdminJoined(
      nameOf(event.text("user")),
      event.text("approved_by").takeIf { it.isNotEmpty() }?.let(nameOf),
    )
    else -> null
  }
}

/** A text field, empty when missing or not text. */
private fun JsonObject?.text(key: String): String = (this?.get(key) as? JsonPrimitive)?.contentOrNull.orEmpty()

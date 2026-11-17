package de.zeitgleich.teamcraft.widget

import java.io.File
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import de.zeitgleich.teamcraft.security.KeystoreBox
import de.zeitgleich.teamcraft.security.SealedCache
import de.zeitgleich.teamcraft.security.SealedFile
import uniffi.teamcraft_ffi.NotificationContent

/** Most rows the widget keeps; a home screen shows a handful, the rest scroll. */
const val WIDGET_MAX_ROWS = 20

/**
 * What the home screen widget shows (issue #2076): the unread conversations as the app saw them
 * last, already cut down to what the privacy settings allow. Only what may show is written, so a
 * stricter setting never leaves names or text on the disk; sealed like the start cache
 * (e2ee.md §9.2) and deleted at sign-out.
 */
@Serializable
data class WidgetSnapshot(
  val privacy: WidgetPrivacy,
  /** Conversations with unread messages, also those not listed (hidden names, over the limit). */
  val unreadConversations: Int,
  /** Empty unless [privacy] shows names. */
  val rows: List<WidgetRow>,
)

/** How much the widget may show, from the device's settings ([widgetPrivacy]). */
@Serializable
enum class WidgetPrivacy {
  /** The app lock is on: nothing personal, not even the number of conversations. */
  LOCKED,

  /** Notifications show nothing: only how many conversations wait. */
  COUNT_ONLY,

  /** The default: names and unread counts. */
  NAMES,

  /** Names, counts and the newest message, when notifications show content and the user asked. */
  PREVIEWS,
}

/** One unread conversation. */
@Serializable
data class WidgetRow(
  val conversationId: String,
  val name: String,
  /** The count as the chat list labels it ("99+"). */
  val countLabel: String,
  /** The newest message, one line with its sender; only with [WidgetPrivacy.PREVIEWS]. */
  val maybePreview: String? = null,
)

/**
 * The widget's privacy: the app lock hides everything personal; otherwise the notification
 * setting decides, and previews need the widget's own opt-in on top of notifications that show
 * content (docs/notifications.md, "Home screen widget").
 */
fun widgetPrivacy(content: NotificationContent, isAppLockOn: Boolean, isPreviewWanted: Boolean): WidgetPrivacy = when {
  isAppLockOn -> WidgetPrivacy.LOCKED
  content == NotificationContent.NOTHING -> WidgetPrivacy.COUNT_ONLY
  content == NotificationContent.CONTENT && isPreviewWanted -> WidgetPrivacy.PREVIEWS
  else -> WidgetPrivacy.NAMES
}

/** The snapshot of [rows] (all unread, in the chat list's order) under [privacy]. */
fun widgetSnapshot(privacy: WidgetPrivacy, rows: List<WidgetRow>): WidgetSnapshot = when (privacy) {
  WidgetPrivacy.LOCKED -> WidgetSnapshot(privacy, unreadConversations = 0, rows = emptyList())
  WidgetPrivacy.COUNT_ONLY -> WidgetSnapshot(privacy, rows.size, emptyList())
  WidgetPrivacy.NAMES -> WidgetSnapshot(privacy, rows.size, rows.take(WIDGET_MAX_ROWS).map { it.copy(maybePreview = null) })
  WidgetPrivacy.PREVIEWS -> WidgetSnapshot(privacy, rows.size, rows.take(WIDGET_MAX_ROWS))
}

/** The sealed file of the [WidgetSnapshot]; none while signed out. */
class WidgetSnapshotStore(private val filesDir: File) {
  private val sealed = SealedCache(
    File(filesDir, FILE_NAME),
    SealedFile(File(filesDir, KEY_FILE_NAME), KeystoreBox(KEY_ALIAS)),
  )

  /** `null` when there is none (signed out, never written) or it can't be read. */
  @Synchronized
  fun read(): WidgetSnapshot? = sealed.read()?.let { bytes ->
    runCatching { json.decodeFromString<WidgetSnapshot>(bytes.decodeToString()) }.getOrNull()
  }

  /** Whether one was written; a cheap check that unseals nothing. */
  fun exists(): Boolean = File(filesDir, FILE_NAME).exists()

  @Synchronized
  fun write(snapshot: WidgetSnapshot) = sealed.write(json.encodeToString(snapshot).encodeToByteArray())

  @Synchronized
  fun delete() = sealed.delete()

  private companion object {
    const val FILE_NAME = "widget.sealed"
    const val KEY_FILE_NAME = "widget.key"
    const val KEY_ALIAS = "teamcraft_widget"
    val json = Json { ignoreUnknownKeys = true }
  }
}

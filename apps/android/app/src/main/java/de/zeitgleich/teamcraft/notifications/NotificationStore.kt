package de.zeitgleich.teamcraft.notifications

import android.content.Context
import android.util.Log
import java.io.File
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import de.zeitgleich.teamcraft.core.WireJson
import de.zeitgleich.teamcraft.security.KeystoreBox
import de.zeitgleich.teamcraft.security.SealedFile

/**
 * The shown conversations' lines, sealed with a Keystore key like the session (they are message
 * text), so a new process continues a conversation's notification. Deleted on sign-out.
 */
class NotificationStore(context: Context) {
  private val file = SealedFile(File(context.noBackupFilesDir, FILE_NAME), KeystoreBox(KEY_ALIAS))

  fun read(): ShownNotifications =
    try {
      file.read()?.let { WireJson.decodeFromString(ShownNotifications.serializer(), it.decodeToString()) }
        ?: ShownNotifications()
    } catch (failure: Exception) {
      // Unreadable (another key after a restore, a newer format): start empty, nothing is lost
      // that the server doesn't still have.
      Log.w(TAG, "shown notifications unreadable: ${failure.javaClass.simpleName}")
      ShownNotifications()
    }

  fun write(shown: ShownNotifications) {
    try {
      if (shown.conversations.isEmpty() && shown.maybeBadge == null) {
        file.delete()
      } else {
        file.write(WireJson.encodeToString(ShownNotifications.serializer(), shown).encodeToByteArray())
      }
    } catch (failure: Exception) {
      // The notification is posted anyway; only a later process would start it afresh.
      Log.w(TAG, "shown notifications not saved: ${failure.javaClass.simpleName}")
    }
  }

  fun delete() {
    file.delete()
  }

  private companion object {
    const val FILE_NAME = "shown_notifications"
    const val KEY_ALIAS = "teamcraft_notifications"
    const val TAG = "TeamCraftNotify"
  }
}

@Serializable
data class ShownNotifications(
  val conversations: Map<String, ConversationNotification> = emptyMap(),
  /** The last badge count a wake-up carried. */
  @SerialName("badge") val maybeBadge: Int? = null,
)

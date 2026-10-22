package de.zeitgleich.teamcraft.notifications

import android.content.Context
import java.security.SecureRandom

/**
 * A random secret of this installation that the app's own notification intents carry. The main
 * activity is exported (it is the launcher), so other apps can start it with extras; a
 * conversation to open is only taken from intents that carry this token.
 */
object LaunchToken {
  private const val FILE = "launch_token"
  private const val KEY = "token"
  private const val TOKEN_BYTES = 32
  /** Snowflake IDs are positive 64-bit numbers. */
  private const val MAX_ID_DIGITS = 19

  fun of(context: Context): String {
    val prefs = context.getSharedPreferences(FILE, Context.MODE_PRIVATE)
    prefs.getString(KEY, null)?.let { return it }
    val bytes = ByteArray(TOKEN_BYTES).also { SecureRandom().nextBytes(it) }
    val token = bytes.joinToString("") { "%02x".format(it) }
    prefs.edit().putString(KEY, token).apply()
    return token
  }

  /** Whether `id` can be a conversation ID (a Snowflake); anything else never reaches navigation. */
  fun isConversationId(id: String): Boolean = id.length in 1..MAX_ID_DIGITS && id.all { it in '0'..'9' }

  /** Whether `maybeToken` is this installation's token (compared in constant time). */
  fun isValid(context: Context, maybeToken: String?): Boolean {
    val token = maybeToken ?: return false
    return java.security.MessageDigest.isEqual(token.toByteArray(), of(context).toByteArray())
  }
}

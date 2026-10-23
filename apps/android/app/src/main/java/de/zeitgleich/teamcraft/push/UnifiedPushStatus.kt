package de.zeitgleich.teamcraft.push

import android.content.Context

/**
 * What the UnifiedPush distributor last told the app about its registration, for the
 * diagnostics: a new endpoint, a failed registration (with the connector's reason) or that it
 * unregistered the app (the distributor was uninstalled or the user removed the app there).
 */
class UnifiedPushStatus(context: Context) {
  private val prefs = context.getSharedPreferences(FILE, Context.MODE_PRIVATE)

  val state: State
    get() = State.entries.firstOrNull { it.name == prefs.getString(KEY_STATE, null) } ?: State.UNKNOWN

  /** The connector's failure reason (an enum name), when [state] is [State.FAILED]. */
  val maybeFailureReason: String? get() = prefs.getString(KEY_REASON, null)

  val maybeChangedAtMs: Long? get() = prefs.getLong(KEY_CHANGED_AT, 0).takeIf { it > 0 }

  fun record(state: State, maybeReason: String? = null) {
    prefs.edit()
      .putString(KEY_STATE, state.name)
      .putString(KEY_REASON, maybeReason)
      .putLong(KEY_CHANGED_AT, System.currentTimeMillis())
      .apply()
  }

  enum class State {
    UNKNOWN,
    REGISTERED,
    FAILED,
    UNREGISTERED,
    TEMPORARILY_UNAVAILABLE,
  }

  private companion object {
    const val FILE = "unifiedpush_status"
    const val KEY_STATE = "state"
    const val KEY_REASON = "reason"
    const val KEY_CHANGED_AT = "changed_at"
  }
}

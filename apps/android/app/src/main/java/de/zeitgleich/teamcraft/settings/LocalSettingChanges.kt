package de.zeitgleich.teamcraft.settings

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.update

/**
 * Tells [SettingsSync] that the user changed a setting that follows the account (#945). Each
 * store of such a setting calls [changed] from its setter; values taken over from the account are
 * written inside [fromAccount], so they don't go back to it as a change of this device.
 */
object LocalSettingChanges {
  private val count = MutableStateFlow(0L)

  @Volatile private var isApplyingAccount = false

  val changes: StateFlow<Long> = count

  fun changed() {
    if (!isApplyingAccount) count.update { it + 1 }
  }

  /** Runs [apply] on the calling thread without reporting its writes as changes of the user. */
  @Synchronized
  fun fromAccount(apply: () -> Unit) {
    isApplyingAccount = true
    try {
      apply()
    } finally {
      isApplyingAccount = false
    }
  }
}

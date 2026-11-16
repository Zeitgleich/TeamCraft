package de.zeitgleich.teamcraft.calls

import android.annotation.SuppressLint
import android.content.Context
import android.os.PowerManager

private const val WAKE_LOCK_TAG = "teamcraft:call-proximity"

/**
 * The screen goes off at the ear during a call, as in the phone app: while the call's sound comes
 * from the earpiece, a proximity wake lock is held, and the system turns the screen off whenever
 * the sensor reads near (and on again when it doesn't). Loudspeaker, headsets and Bluetooth keep
 * the screen on. Without such a lock (no sensor) nothing changes.
 */
class CallProximity(context: Context) {
  private val maybeLock: PowerManager.WakeLock? =
    context.getSystemService(PowerManager::class.java)?.let { power ->
      if (power.isWakeLockLevelSupported(PowerManager.PROXIMITY_SCREEN_OFF_WAKE_LOCK)) {
        power.newWakeLock(PowerManager.PROXIMITY_SCREEN_OFF_WAKE_LOCK, WAKE_LOCK_TAG).apply { setReferenceCounted(false) }
      } else {
        null
      }
    }

  /** Follows the call: whether one runs on this device, and whether its sound is on the earpiece. */
  @SuppressLint("WakelockTimeout") // Released when the call leaves the earpiece or ends.
  @Synchronized
  fun update(isInCall: Boolean, isOnEarpiece: Boolean) {
    val lock = maybeLock ?: return
    val wantsLock = isScreenOffAtEar(isInCall, isOnEarpiece)
    if (wantsLock && !lock.isHeld) lock.acquire()
    // The screen comes back at once, also when the phone is still near (a hang-up at the ear).
    if (!wantsLock && lock.isHeld) lock.release()
  }
}

/** The screen may go off at the ear only during a call whose sound is on the earpiece. */
fun isScreenOffAtEar(isInCall: Boolean, isOnEarpiece: Boolean): Boolean = isInCall && isOnEarpiece

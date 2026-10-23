package de.zeitgleich.teamcraft.security

import android.content.Context
import androidx.biometric.BiometricManager
import androidx.biometric.BiometricManager.Authenticators.BIOMETRIC_WEAK
import androidx.biometric.BiometricManager.Authenticators.DEVICE_CREDENTIAL
import androidx.biometric.BiometricPrompt
import androidx.core.content.ContextCompat
import androidx.fragment.app.FragmentActivity

/** Fingerprint, face or the device's PIN, pattern or password; whatever the device offers. */
const val APP_LOCK_AUTHENTICATORS = BIOMETRIC_WEAK or DEVICE_CREDENTIAL

/** How long the app may be in the background before it asks to unlock again. */
enum class LockAfter(val millis: Long) {
  Immediately(0),
  OneMinute(60_000),
  FiveMinutes(5 * 60_000),
  FifteenMinutes(15 * 60_000),
}

/**
 * "App lock": the chats open only after the device's biometrics or screen lock. A device setting,
 * off by default. It guards an unlocked phone handed to someone else; the data itself is protected
 * by the Keystore-sealed store either way.
 */
class AppLockPrefs private constructor(context: Context) {
  private val prefs = context.getSharedPreferences(FILE, Context.MODE_PRIVATE)

  val isEnabled: Boolean get() = prefs.getBoolean(KEY_ENABLED, false)

  val lockAfter: LockAfter
    get() = LockAfter.entries.firstOrNull { it.name == prefs.getString(KEY_AFTER, null) } ?: DEFAULT_LOCK_AFTER

  fun setEnabled(isOn: Boolean) {
    prefs.edit().putBoolean(KEY_ENABLED, isOn).apply()
  }

  fun setLockAfter(after: LockAfter) {
    prefs.edit().putString(KEY_AFTER, after.name).apply()
  }

  companion object {
    private const val FILE = "app_lock_prefs"
    private const val KEY_ENABLED = "enabled"
    private const val KEY_AFTER = "lock_after"
    val DEFAULT_LOCK_AFTER = LockAfter.OneMinute

    fun prefs(context: Context) = AppLockPrefs(context.applicationContext)

    /** Whether the device has a screen lock or biometrics the app lock can use. */
    fun isAvailable(context: Context): Boolean =
      BiometricManager.from(context).canAuthenticate(APP_LOCK_AUTHENTICATORS) == BiometricManager.BIOMETRIC_SUCCESS
  }
}

/**
 * Decides when the app is locked: at a cold start, and when it comes back after more than
 * [AppLockPrefs.lockAfter] in the background. Times are from the monotonic clock.
 */
class AppLockState(private val now: () -> Long) {
  private var maybeBackgroundedAt: Long? = null

  fun onBackground() {
    maybeBackgroundedAt = now()
  }

  /** Whether the app must lock now that it is in the foreground again. */
  fun shouldLockOnForeground(isEnabled: Boolean, lockAfter: LockAfter): Boolean {
    val backgroundedAt = maybeBackgroundedAt ?: return false
    maybeBackgroundedAt = null
    return isEnabled && now() - backgroundedAt >= lockAfter.millis
  }
}

/** Shows the system prompt; `onUnlocked` runs on success. Cancelling keeps the app locked. */
fun promptUnlock(activity: FragmentActivity, title: String, onUnlocked: () -> Unit) {
  if (BiometricManager.from(activity).canAuthenticate(APP_LOCK_AUTHENTICATORS) != BiometricManager.BIOMETRIC_SUCCESS) {
    // The screen lock was removed since the app lock was turned on: there is nothing to check
    // against, and locking the user out of their chats would be worse.
    onUnlocked()
    return
  }
  val prompt = BiometricPrompt(
    activity,
    ContextCompat.getMainExecutor(activity),
    object : BiometricPrompt.AuthenticationCallback() {
      override fun onAuthenticationSucceeded(result: BiometricPrompt.AuthenticationResult) = onUnlocked()
    },
  )
  prompt.authenticate(
    BiometricPrompt.PromptInfo.Builder()
      .setTitle(title)
      .setAllowedAuthenticators(APP_LOCK_AUTHENTICATORS)
      .build(),
  )
}

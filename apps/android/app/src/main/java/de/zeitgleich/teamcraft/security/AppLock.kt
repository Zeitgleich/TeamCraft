package de.zeitgleich.teamcraft.security

import android.content.Context
import android.os.Build
import android.util.Log
import androidx.biometric.BiometricManager
import androidx.biometric.BiometricManager.Authenticators.BIOMETRIC_STRONG
import androidx.biometric.BiometricManager.Authenticators.BIOMETRIC_WEAK
import androidx.biometric.BiometricManager.Authenticators.DEVICE_CREDENTIAL
import androidx.biometric.BiometricPrompt
import androidx.core.content.ContextCompat
import androidx.fragment.app.FragmentActivity

private const val TAG = "AppLock"

/**
 * Fingerprint, face or the device's PIN, pattern or password. From Android 11 only strong
 * biometrics, as only they authorize a Keystore operation ([AppLockKey]); a weak face unlock asks
 * for the screen lock instead. Before, Android can't combine the screen lock with a key operation
 * in this prompt, so any biometrics count there.
 */
val APP_LOCK_AUTHENTICATORS =
  if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) BIOMETRIC_STRONG or DEVICE_CREDENTIAL else BIOMETRIC_WEAK or DEVICE_CREDENTIAL

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

/**
 * A key whose use proves that the system prompt succeeded. [T] is what the prompt authorizes (a
 * `Cipher` on the device), so the gate can be tested without a Keystore.
 */
interface UnlockKey<T> {
  /** A fresh challenge for one prompt; creates the key when there is none (also after an update). */
  fun begin(): T

  /** Whether [maybeProof], from the prompt's result, is [challenge] authorized by the prompt. */
  fun isProof(challenge: T, maybeProof: T?): Boolean

  fun reset()
}

/** The key can't be used any more: the screen lock was removed, or replaced since. */
class UnlockKeyInvalidatedException(cause: Throwable) : Exception(cause)

/** How an unlock starts. */
sealed interface UnlockStart<out T> {
  /** No screen lock any more: there is nothing to check against, so the app opens. */
  data object Open : UnlockStart<Nothing>

  /** The prompt; its success counts only with a proof for [challenge]. */
  data class Prompt<T>(val challenge: T) : UnlockStart<T>

  /** The Keystore failed: the prompt alone decides, as before #1200, so nobody is locked out. */
  data class PromptUnbound(val failure: Exception) : UnlockStart<Nothing>
}

/**
 * Ties the app lock to [key] (#1200). The device having no screen lock any more still opens the
 * app (locking the user out of their chats would be worse), and a key the device invalidated is
 * replaced before the prompt; neither lets a forged success through.
 */
class AppLockGate<T>(private val key: UnlockKey<T>, private val hasScreenLock: () -> Boolean) {
  fun start(): UnlockStart<T> {
    if (!hasScreenLock()) {
      // The device already made the key unusable; the next screen lock gets a new one.
      key.reset()
      return UnlockStart.Open
    }
    return try {
      UnlockStart.Prompt(beginReplacingInvalidated())
    } catch (failure: Exception) {
      UnlockStart.PromptUnbound(failure)
    }
  }

  fun isUnlocked(challenge: T, maybeProof: T?): Boolean = key.isProof(challenge, maybeProof)

  private fun beginReplacingInvalidated(): T =
    try {
      key.begin()
    } catch (invalidated: UnlockKeyInvalidatedException) {
      key.reset()
      key.begin()
    }
}

/**
 * Shows the system prompt; `onUnlocked` runs on success. Cancelling keeps the app locked. From
 * Android 11 the success must come with the Keystore operation of [AppLockKey].
 */
fun promptUnlock(activity: FragmentActivity, title: String, onUnlocked: () -> Unit) {
  val hasScreenLock = {
    BiometricManager.from(activity).canAuthenticate(APP_LOCK_AUTHENTICATORS) == BiometricManager.BIOMETRIC_SUCCESS
  }
  if (Build.VERSION.SDK_INT < Build.VERSION_CODES.R) {
    // Android 8 to 10: a gate in the user interface only (e2ee.md §9.2, "App lock").
    if (hasScreenLock()) showPrompt(activity, title, maybeCryptoObject = null) { onUnlocked() } else onUnlocked()
    return
  }
  val gate = AppLockGate(AppLockKey(), hasScreenLock)
  when (val start = gate.start()) {
    UnlockStart.Open -> onUnlocked()
    is UnlockStart.PromptUnbound -> {
      Log.w(TAG, "app lock key unusable, the prompt alone decides: ${start.failure.javaClass.simpleName}")
      showPrompt(activity, title, maybeCryptoObject = null) { onUnlocked() }
    }
    is UnlockStart.Prompt -> showPrompt(activity, title, BiometricPrompt.CryptoObject(start.challenge)) { result ->
      if (gate.isUnlocked(start.challenge, result.cryptoObject?.cipher)) onUnlocked() else Log.w(TAG, "unlock without a valid key proof refused")
    }
  }
}

private fun showPrompt(
  activity: FragmentActivity,
  title: String,
  maybeCryptoObject: BiometricPrompt.CryptoObject?,
  onSucceeded: (BiometricPrompt.AuthenticationResult) -> Unit,
) {
  val prompt = BiometricPrompt(
    activity,
    ContextCompat.getMainExecutor(activity),
    object : BiometricPrompt.AuthenticationCallback() {
      override fun onAuthenticationSucceeded(result: BiometricPrompt.AuthenticationResult) = onSucceeded(result)
    },
  )
  val info = BiometricPrompt.PromptInfo.Builder()
    .setTitle(title)
    .setAllowedAuthenticators(APP_LOCK_AUTHENTICATORS)
    .build()
  if (maybeCryptoObject == null) prompt.authenticate(info) else prompt.authenticate(info, maybeCryptoObject)
}

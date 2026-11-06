package de.zeitgleich.teamcraft.calls

import android.content.Context
import android.util.Log

/**
 * Hands the Java VM and the app's context to the Rust core's audio (cpal's AAudio backend, through
 * ndk-context), which needs them to open the microphone and speaker. Without it every call ended
 * the moment it connected. The core library is loaded through JNA; JNI finds the same library here.
 */
object AudioContext {
  @Volatile private var isReady = false

  /** Once per process, before the first call opens its audio; cheap afterwards. */
  fun ensure(context: Context) {
    if (isReady) return
    synchronized(this) {
      if (isReady) return
      runCatching {
        System.loadLibrary(LIBRARY)
        initialize(context.applicationContext)
      }.onSuccess { isSetUp ->
        // Not set up: the next call tries again (the core keeps no half-done state).
        isReady = isSetUp
        if (!isSetUp) Log.w(TAG, "audio context not set up")
      }.onFailure { Log.w(TAG, "audio context not set up: ${it.javaClass.simpleName}") }
    }
  }

  /** `true` once the core has the context, by this call or an earlier one. */
  @JvmStatic private external fun initialize(context: Context): Boolean

  private const val LIBRARY = "teamcraft_ffi"
  private const val TAG = "TeamCraftCalls"
}

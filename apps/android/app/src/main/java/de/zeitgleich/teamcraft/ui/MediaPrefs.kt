package de.zeitgleich.teamcraft.ui

import android.content.Context
import android.net.ConnectivityManager
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import de.zeitgleich.teamcraft.ui.media.MediaQuality
import uniffi.teamcraft_ffi.OriginalDownload
import uniffi.teamcraft_ffi.fetchesOriginalAutomatically

/**
 * When the image viewer fetches a picture's original by itself (Settings → Chats, #240). Kept on
 * this device: the data plan is the phone's, not the account's. The thumbnail shows at once either
 * way, and "Load original" fetches it on demand.
 */
class MediaPrefs private constructor(private val context: Context) {
  private val prefs = context.getSharedPreferences(FILE, Context.MODE_PRIVATE)
  private val animatedImagesFlow = sharedAnimatedImages ?: MutableStateFlow(
    animatedImagesOf(prefs.getString(KEY_ANIMATED_IMAGES, null)),
  ).also { sharedAnimatedImages = it }

  /** When animated images move. Kept on this device, like the mobile apps' other settings. */
  val animatedImages: StateFlow<AnimatedImagesMode> get() = animatedImagesFlow

  fun setAnimatedImages(choice: AnimatedImagesMode) {
    prefs.edit().putString(KEY_ANIMATED_IMAGES, choice.name).apply()
    animatedImagesFlow.value = choice
  }

  val originalDownload: OriginalDownload get() = originalDownloadOf(prefs.getString(KEY_ORIGINAL_DOWNLOAD, null))

  fun setOriginalDownload(choice: OriginalDownload) {
    prefs.edit().putString(KEY_ORIGINAL_DOWNLOAD, choice.name).apply()
  }

  /** Whether opening a picture fetches its original now: the setting on the current network. */
  fun fetchesOriginalNow(): Boolean {
    // No answer counts as metered: better to wait for a tap than to spend mobile data.
    val isMetered = context.getSystemService(ConnectivityManager::class.java)?.isActiveNetworkMetered ?: true
    return fetchesOriginalAutomatically(originalDownload, isMetered)
  }

  /**
   * The quality photos and videos are sent at unless changed while sending (#1358), Standard until
   * set. Kept on this device, like the data plan it is about.
   */
  val sendQuality: MediaQuality get() = MediaQuality.entries.firstOrNull { it.name == prefs.getString(KEY_SEND_QUALITY, null) } ?: MediaQuality.Standard

  fun setSendQuality(choice: MediaQuality) {
    prefs.edit().putString(KEY_SEND_QUALITY, choice.name).apply()
  }

  companion object {
    private const val FILE = "media_prefs"
    private const val KEY_SEND_QUALITY = "send_quality"
    private const val KEY_ORIGINAL_DOWNLOAD = "original_download"
    private const val KEY_ANIMATED_IMAGES = "animated_images"

    /** One value for the whole process, so an open conversation follows a change in Settings. */
    private var sharedAnimatedImages: MutableStateFlow<AnimatedImagesMode>? = null

    fun prefs(context: Context) = MediaPrefs(context.applicationContext)
  }
}

/**
 * The stored choice. Nothing stored (or something unknown) is the default, Wi-Fi only: opening
 * pictures never spends mobile data unasked. An explicit choice is kept as it is.
 */
internal fun originalDownloadOf(maybeStored: String?): OriginalDownload =
  OriginalDownload.entries.firstOrNull { it.name == maybeStored } ?: OriginalDownload.UNMETERED_ONLY

/** The stored choice; nothing (or something unknown) plays automatically. */
internal fun animatedImagesOf(maybeStored: String?): AnimatedImagesMode =
  AnimatedImagesMode.entries.firstOrNull { it.name == maybeStored } ?: AnimatedImagesMode.AUTOPLAY

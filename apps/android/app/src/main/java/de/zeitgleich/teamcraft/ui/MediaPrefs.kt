package de.zeitgleich.teamcraft.ui

import android.content.Context
import android.net.ConnectivityManager
import uniffi.teamcraft_ffi.OriginalDownload
import uniffi.teamcraft_ffi.fetchesOriginalAutomatically

/**
 * When the image viewer fetches a picture's original by itself (Settings → Chats, #240). Kept on
 * this device: the data plan is the phone's, not the account's. The thumbnail shows at once either
 * way, and "Load original" fetches it on demand.
 */
class MediaPrefs private constructor(private val context: Context) {
  private val prefs = context.getSharedPreferences(FILE, Context.MODE_PRIVATE)

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

  companion object {
    private const val FILE = "media_prefs"
    private const val KEY_ORIGINAL_DOWNLOAD = "original_download"

    fun prefs(context: Context) = MediaPrefs(context.applicationContext)
  }
}

/**
 * The stored choice. Nothing stored (or something unknown) is the default, Wi-Fi only: opening
 * pictures never spends mobile data unasked. An explicit choice is kept as it is.
 */
internal fun originalDownloadOf(maybeStored: String?): OriginalDownload =
  OriginalDownload.entries.firstOrNull { it.name == maybeStored } ?: OriginalDownload.UNMETERED_ONLY

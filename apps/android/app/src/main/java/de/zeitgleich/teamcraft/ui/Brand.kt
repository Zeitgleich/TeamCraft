package de.zeitgleich.teamcraft.ui

import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.util.Log
import java.io.File
import java.io.IOException
import java.net.HttpURLConnection
import java.net.URL
import java.util.Locale
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import de.zeitgleich.teamcraft.server.StoragePaths
import de.zeitgleich.teamcraft.server.readAtMost
import uniffi.teamcraft_ffi.LoginBrand
import uniffi.teamcraft_ffi.loginBrand

private const val TAG = "Brand"
private const val PREFS = "brand"
private const val MAX_BRAND_JSON_BYTES = 64 * 1024
private const val MAX_LOGO_BYTES = 512 * 1024
private const val TIMEOUT_MILLIS = 10_000

/**
 * The server's brand for the sign-in screen (docs/branding.md §5): asked without credentials from
 * the server the user picked, checked by the core, cached per server origin so the next start
 * shows it at once. Brand data only changes what the screen shows, never which server the app
 * talks to.
 */
class BrandStore(context: Context, val origin: String, storageKey: String) {
  private val ssoIconFile = File(context.filesDir, StoragePaths.ssoIconFile(storageKey))
  private val prefs = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
  private val directory = File(context.filesDir, StoragePaths.brandDir(storageKey))
  private val key = "teamcraft.brand:" + origin.trimEnd('/')

  /** The brand of the last start, if this server sent one. */
  fun cached(): LoginBrand? = prefs.getString(key, null)?.let { check(it) }

  /** Asks the server again; the cached brand stays when it can't be reached. */
  suspend fun refresh(): LoginBrand? = withContext(Dispatchers.IO) {
    val json = try {
      download(origin.trimEnd('/') + "/api/v1/brand", MAX_BRAND_JSON_BYTES).decodeToString()
    } catch (failure: IOException) {
      Log.i(TAG, "brand not reachable: ${failure.javaClass.simpleName}")
      return@withContext cached()
    }
    val brand = check(json) ?: return@withContext null
    prefs.edit().putString(key, json).apply()
    saveImage(brand.maybeLogoUrl, logoFile(isDark = false))
    saveImage(brand.maybeLogoDarkUrl, logoFile(isDark = true))
    saveImage(brand.maybeIconUrl, iconFile(isDark = false))
    saveImage(brand.maybeIconDarkUrl, iconFile(isDark = true))
    brand
  }

  /** The cached logo for the theme, decoded; null without one. */
  fun logo(isDark: Boolean): Bitmap? = decoded(logoFile(isDark))

  /** The cached square icon for the theme (the brand's tile on the sign-in screen); null without one. */
  fun icon(isDark: Boolean): Bitmap? = decoded(iconFile(isDark))

  /** The cached icon of the identity provider ("Sign in with …"), decoded; null without one. */
  fun ssoIcon(): Bitmap? = ssoIconFile.takeIf(File::exists)?.let { BitmapFactory.decodeFile(it.path) }

  /**
   * Caches the icon the server's sign-in page names (the core only lets the brand endpoint of
   * this server through); [maybeUrl] null removes it. The cached icon stays when it can't be
   * fetched.
   */
  suspend fun refreshSsoIcon(maybeUrl: String?) = withContext(Dispatchers.IO) {
    if (maybeUrl == null || !maybeUrl.startsWith(origin.trimEnd('/') + "/")) {
      ssoIconFile.delete()
      return@withContext
    }
    try {
      val bytes = download(maybeUrl, MAX_LOGO_BYTES)
      directory.mkdirs()
      ssoIconFile.writeBytes(bytes)
    } catch (failure: IOException) {
      Log.i(TAG, "sign-in icon not reachable: ${failure.javaClass.simpleName}")
    }
  }

  private fun check(json: String): LoginBrand? = loginBrand(json, origin, Locale.getDefault().toLanguageTag())

  private fun decoded(file: File): Bitmap? = file.takeIf(File::exists)?.let { BitmapFactory.decodeFile(it.path) }

  private fun saveImage(maybeUrl: String?, file: File) {
    if (maybeUrl == null) {
      file.delete()
      return
    }
    try {
      val bytes = download(maybeUrl, MAX_LOGO_BYTES)
      directory.mkdirs()
      file.writeBytes(bytes)
    } catch (failure: IOException) {
      // The previous image (or none) stays; the name is shown either way.
      Log.i(TAG, "brand image not reachable: ${failure.javaClass.simpleName}")
    }
  }

  private fun logoFile(isDark: Boolean) = File(directory, if (isDark) "logo_dark" else "logo")

  private fun iconFile(isDark: Boolean) = File(directory, if (isDark) "icon_dark" else "icon")
}

private fun download(url: String, maxBytes: Int): ByteArray {
  val connection = URL(url).openConnection() as HttpURLConnection
  connection.connectTimeout = TIMEOUT_MILLIS
  connection.readTimeout = TIMEOUT_MILLIS
  connection.instanceFollowRedirects = false
  try {
    if (connection.responseCode != HttpURLConnection.HTTP_OK) throw IOException("status ${connection.responseCode}")
    return connection.inputStream.use { readAtMost(it, maxBytes) }
  } finally {
    connection.disconnect()
  }
}

/** The brand's square icon in both themes, decoded (null: the brand has none for that theme). */
class BrandIcons(val light: Bitmap?, val dark: Bitmap?) {
  fun forTheme(isDark: Boolean): Bitmap? = if (isDark) dark ?: light else light
}

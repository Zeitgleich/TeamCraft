package de.zeitgleich.teamcraft.ui

import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.util.Log
import java.io.ByteArrayOutputStream
import java.io.File
import java.io.IOException
import java.io.InputStream
import java.net.HttpURLConnection
import java.net.URL
import java.util.Locale
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import uniffi.teamcraft_ffi.LoginBrand
import uniffi.teamcraft_ffi.loginBrand

private const val TAG = "Brand"
private const val PREFS = "brand"
private const val MAX_BRAND_JSON_BYTES = 64 * 1024
private const val MAX_LOGO_BYTES = 512 * 1024
private const val TIMEOUT_MILLIS = 10_000

/**
 * The server's brand for the sign-in screen (docs/branding.md §5): asked without credentials from
 * the configured server, checked by the core, cached per server URL so the next start shows it at
 * once. Brand data only changes what the screen shows, never which server the app talks to.
 */
class BrandStore(context: Context, private val serverUrl: String) {
  private val prefs = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
  private val directory = File(context.filesDir, "brand")
  private val key = "teamcraft.brand:" + serverUrl.trimEnd('/')

  /** The brand of the last start, if this server sent one. */
  fun cached(): LoginBrand? = prefs.getString(key, null)?.let { check(it) }

  /** Asks the server again; the cached brand stays when it can't be reached. */
  suspend fun refresh(): LoginBrand? = withContext(Dispatchers.IO) {
    val json = try {
      download(serverUrl.trimEnd('/') + "/api/v1/brand", MAX_BRAND_JSON_BYTES).decodeToString()
    } catch (failure: IOException) {
      Log.i(TAG, "brand not reachable: ${failure.javaClass.simpleName}")
      return@withContext cached()
    }
    val brand = check(json) ?: return@withContext null
    prefs.edit().putString(key, json).apply()
    saveLogo(brand.maybeLogoUrl, isDark = false)
    saveLogo(brand.maybeLogoDarkUrl, isDark = true)
    brand
  }

  /** The cached logo for the theme, decoded; null without one. */
  fun logo(isDark: Boolean): Bitmap? {
    val file = logoFile(isDark).takeIf(File::exists) ?: return null
    return BitmapFactory.decodeFile(file.path)
  }

  private fun check(json: String): LoginBrand? = loginBrand(json, serverUrl, Locale.getDefault().toLanguageTag())

  private fun saveLogo(maybeUrl: String?, isDark: Boolean) {
    val file = logoFile(isDark)
    if (maybeUrl == null) {
      file.delete()
      return
    }
    try {
      val bytes = download(maybeUrl, MAX_LOGO_BYTES)
      directory.mkdirs()
      file.writeBytes(bytes)
    } catch (failure: IOException) {
      // The previous logo (or none) stays; the name is shown either way.
      Log.i(TAG, "logo not reachable: ${failure.javaClass.simpleName}")
    }
  }

  private fun logoFile(isDark: Boolean) = File(directory, if (isDark) "logo_dark" else "logo")
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

private fun readAtMost(input: InputStream, maxBytes: Int): ByteArray {
  val output = ByteArrayOutputStream()
  val buffer = ByteArray(8 * 1024)
  while (true) {
    val count = input.read(buffer)
    if (count < 0) return output.toByteArray()
    if (output.size() + count > maxBytes) throw IOException("too large")
    output.write(buffer, 0, count)
  }
}

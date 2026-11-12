package de.zeitgleich.teamcraft.ui

import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.util.Log
import android.widget.Toast
import androidx.annotation.StringRes
import androidx.core.content.FileProvider
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.crash.CrashSdks
import java.io.File
import java.io.IOException

/**
 * The files the app hands to other apps through its FileProvider (issue #1761): decrypted downloads
 * and the diagnostics export. One place knows where they lie; `res/xml/shared_files.xml` must list
 * exactly [ROOTS] (`SharedFilesTest` reads the XML and compares), so the two can't drift.
 *
 * The core writes downloads to `<data dir>/downloads/<account>/`, and the data dir of a server is
 * `files/store/<server>` ([de.zeitgleich.teamcraft.server.StoragePaths.storeDir]). The folder name
 * is `DOWNLOADS_DIR` in `crates/teamcraft-ffi/src/engine.rs`.
 */
object SharedFiles {
  /** The provider's authority is the application ID plus this. */
  private const val AUTHORITY_SUFFIX = ".files"

  /** Where the core keeps the decrypted downloads inside a server's data dir. */
  const val DOWNLOADS_DIR = "downloads"

  /** The folder of the diagnostics export in the app cache. */
  const val DIAGNOSTICS_DIR = "diagnostics"

  /** The parent of every server's data dir, below `filesDir`. */
  const val STORE_DIR = "store"

  private const val TAG = "SharedFiles"

  /** A root of `shared_files.xml`: its element, its `name` and its `path` (with a trailing slash). */
  data class Root(val element: String, val name: String, val path: String)

  /**
   * What `shared_files.xml` lists. The downloads lie below `store/<server>/downloads`, a name per
   * server that XML can't spell, so the whole store is a root and [isShareable] narrows it. A URI
   * exists only for a file the app itself handed to [FileProvider].
   */
  val ROOTS = listOf(
    Root("files-path", "store", "$STORE_DIR/"),
    Root("cache-path", "diagnostics", "$DIAGNOSTICS_DIR/"),
  )

  /** Whether [file] may go to another app: a download of any server, or the diagnostics export. */
  fun isShareable(filesDir: File, cacheDir: File, file: File): Boolean {
    val canonical = try {
      file.canonicalFile
    } catch (failure: IOException) {
      return false
    }
    val diagnostics = File(cacheDir, DIAGNOSTICS_DIR).canonicalFile
    if (canonical != diagnostics && canonical.startsWith(diagnostics)) return true
    // store/<server>/downloads/<something>
    val store = File(filesDir, STORE_DIR).canonicalFile
    if (!canonical.startsWith(store)) return false
    val parts = canonical.relativeTo(store).path.split(File.separatorChar)
    return parts.size >= 3 && parts[1] == DOWNLOADS_DIR && parts.none { it.isEmpty() || it == ".." }
  }

  /** The content URI of [file]; throws [IllegalArgumentException] for one outside the shared folders. */
  fun uriFor(context: Context, file: File): Uri {
    require(isShareable(context.filesDir, context.cacheDir, file)) { "not a shared folder" }
    return FileProvider.getUriForFile(context, context.packageName + AUTHORITY_SUFFIX, file)
  }

  /**
   * Runs [handOff] (making the URI and starting the other app). A failure never crashes the app:
   * the user gets [failureText], and a bug (a file outside the roots, a bad URI) is reported as a
   * handled error without the path. No app for the file is the user's setup, not a bug.
   */
  fun guard(context: Context, kind: String, @StringRes failureText: Int = R.string.file_open_failed, handOff: () -> Unit) {
    try {
      handOff()
    } catch (missing: ActivityNotFoundException) {
      Log.i(TAG, "$kind: no app for the file")
      Toast.makeText(context, R.string.file_open_no_app, Toast.LENGTH_SHORT).show()
    } catch (failure: Exception) {
      Log.w(TAG, "$kind failed: ${failure.javaClass.simpleName}")
      // The message names the file; only the kind and the exception's class are reported.
      CrashSdks.platform().captureHandled("file hand-off: $kind", failure.javaClass.name)
      Toast.makeText(context, failureText, Toast.LENGTH_SHORT).show()
    }
  }

  /** Opens [file] in another app. */
  fun open(context: Context, file: File, mediaType: String) = guard(context, "open") {
    val intent = Intent(Intent.ACTION_VIEW)
      .setDataAndType(uriFor(context, file), mediaType)
      .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
    context.startActivity(Intent.createChooser(intent, file.name))
  }

  /** Sends [file] to another app (share sheet, save to files). */
  fun share(context: Context, file: File, mediaType: String) = guard(context, "share") {
    val intent = Intent(Intent.ACTION_SEND)
      .setType(mediaType)
      .putExtra(Intent.EXTRA_STREAM, uriFor(context, file))
      .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
    context.startActivity(Intent.createChooser(intent, file.name))
  }
}

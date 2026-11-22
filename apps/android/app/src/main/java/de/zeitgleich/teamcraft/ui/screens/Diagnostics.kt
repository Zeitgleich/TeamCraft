package de.zeitgleich.teamcraft.ui.screens

import android.content.Context
import android.content.Intent
import android.os.Build
import de.zeitgleich.teamcraft.BuildConfig
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.TeamCraftApplication
import de.zeitgleich.teamcraft.notifications.NotificationSetup
import de.zeitgleich.teamcraft.ui.SharedFiles
import java.io.File
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import uniffi.teamcraft_ffi.diagnosticsText

/** Where the exported diagnostics wait for the share sheet (app cache, see `shared_files.xml`). */
private const val DIAGNOSTICS_DIR = SharedFiles.DIAGNOSTICS_DIR

/** The OS line of the export, e.g. `Android 15 (API 35), Google Pixel 9`. */
internal fun osVersionLine(): String =
  "Android ${Build.VERSION.RELEASE} (API ${Build.VERSION.SDK_INT}), ${Build.MANUFACTURER} ${Build.MODEL}"

/**
 * Hands the app version, OS version and the core's recent log lines to the share sheet as a text
 * file (issue #1094). The user picks the target; nothing leaves the device before that tap.
 */
suspend fun exportDiagnostics(context: Context) {
  val pushPaths = TeamCraftApplication.graph(context).pushStats.summary()
  // The file is written off the main thread (#2184); a failure reaches the user through the guard.
  val written = withContext(Dispatchers.IO) {
    runCatching {
      val dir = File(context.cacheDir, DIAGNOSTICS_DIR).apply { mkdirs() }
      File(dir, "teamcraft-diagnostics.txt").apply {
        writeText(
          diagnosticsText(BuildConfig.VERSION_NAME, osVersionLine()) +
            "\nNotification paths: $pushPaths\n" +
            NotificationSetup.describe(context),
        )
      }
    }
  }
  SharedFiles.guard(context, "diagnostics", R.string.export_diagnostics_failed) {
    val file = written.getOrThrow()
    val uri = SharedFiles.uriFor(context, file)
    val intent = Intent(Intent.ACTION_SEND)
      .setType("text/plain")
      .putExtra(Intent.EXTRA_STREAM, uri)
      .putExtra(Intent.EXTRA_SUBJECT, context.getString(R.string.export_diagnostics_share_title))
      .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
    context.startActivity(Intent.createChooser(intent, context.getString(R.string.export_diagnostics_share_title)))
  }
}

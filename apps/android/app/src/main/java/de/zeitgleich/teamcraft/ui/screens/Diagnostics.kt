package de.zeitgleich.teamcraft.ui.screens

import android.content.Context
import android.content.Intent
import android.os.Build
import android.widget.Toast
import androidx.core.content.FileProvider
import de.zeitgleich.teamcraft.BuildConfig
import de.zeitgleich.teamcraft.R
import java.io.File
import uniffi.teamcraft_ffi.diagnosticsText

/** Where the exported diagnostics wait for the share sheet (app cache, see `shared_files.xml`). */
private const val DIAGNOSTICS_DIR = "diagnostics"

/** The OS line of the export, e.g. `Android 15 (API 35), Google Pixel 9`. */
internal fun osVersionLine(): String =
  "Android ${Build.VERSION.RELEASE} (API ${Build.VERSION.SDK_INT}), ${Build.MANUFACTURER} ${Build.MODEL}"

/**
 * Hands the app version, OS version and the core's recent log lines to the share sheet as a text
 * file (issue #1094). The user picks the target; nothing leaves the device before that tap.
 */
fun exportDiagnostics(context: Context) {
  val shared = runCatching {
    val dir = File(context.cacheDir, DIAGNOSTICS_DIR).apply { mkdirs() }
    val file = File(dir, "teamcraft-diagnostics.txt")
    file.writeText(diagnosticsText(BuildConfig.VERSION_NAME, osVersionLine()))
    val uri = FileProvider.getUriForFile(context, "${context.packageName}.files", file)
    val intent = Intent(Intent.ACTION_SEND)
      .setType("text/plain")
      .putExtra(Intent.EXTRA_STREAM, uri)
      .putExtra(Intent.EXTRA_SUBJECT, context.getString(R.string.export_diagnostics_share_title))
      .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
    context.startActivity(Intent.createChooser(intent, context.getString(R.string.export_diagnostics_share_title)))
  }.isSuccess
  if (!shared) Toast.makeText(context, R.string.export_diagnostics_failed, Toast.LENGTH_SHORT).show()
}

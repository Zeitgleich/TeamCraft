package de.zeitgleich.teamcraft.ui

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import java.io.File
import java.util.Locale

/** The largest part of a text file the preview reads (the same default as desktop and web, #821). */
const val TEXT_PREVIEW_MAX_BYTES = 10L * 1024 * 1024

private val TEXT_EXTENSIONS = setOf("txt", "csv", "tsv", "log", "md", "text", "tab")

/**
 * Whether a downloaded file opens in the in-app text preview (#2342): plain text and delimited
 * files. Other text types (HTML, scripts) are shown as the text they are; the preview never
 * interprets a file. Binary files turn out as such when the core reads them, and go to another app.
 */
fun isTextPreviewable(fileName: String, mediaType: String): Boolean {
  val type = mediaType.substringBefore(';').trim().lowercase(Locale.ROOT)
  if (type.startsWith("text/")) return true
  if (type == "application/csv" || type == "application/x-csv" || type == "application/vnd.ms-excel" && fileName.isDelimitedName()) return true
  val isGeneric = type.isEmpty() || type == "application/octet-stream"
  return isGeneric && fileName.substringAfterLast('.', "").lowercase(Locale.ROOT) in TEXT_EXTENSIONS
}

private fun String.isDelimitedName(): Boolean = substringAfterLast('.', "").lowercase(Locale.ROOT) in setOf("csv", "tsv")

/**
 * The kind of file a remembered choice (view, separator, header, wrapping) belongs to: its
 * extension, else its media type, so every `.csv` shares them.
 */
fun textPreviewTypeKey(fileName: String, mediaType: String): String {
  val extension = fileName.substringAfterLast('.', "").lowercase(Locale.ROOT)
  return extension.ifEmpty { mediaType.substringBefore(';').trim().lowercase(Locale.ROOT) }
}

/** The preview the app shows now, if any. Set by [openFile]; drawn once by `TextPreviewHost`. */
object TextPreviewRequest {
  /** [fileName] is the name the sender gave; the file on disk may be named otherwise. */
  data class Target(val file: File, val mediaType: String, val fileName: String)

  var current by mutableStateOf<Target?>(null)
    private set

  fun show(file: File, mediaType: String, fileName: String) {
    current = Target(file, mediaType, fileName)
  }

  fun dismiss() {
    current = null
  }
}

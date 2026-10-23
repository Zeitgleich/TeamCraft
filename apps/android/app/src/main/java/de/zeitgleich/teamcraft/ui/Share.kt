package de.zeitgleich.teamcraft.ui

import android.content.Intent
import android.net.Uri
import android.os.Build
import kotlinx.coroutines.flow.MutableStateFlow

/** Most files one share brings into the composer; more is almost certainly a mistake. */
const val MAX_SHARED_FILES = 10

/** Text and files another app shared into TeamCraft, waiting for the user to pick a conversation. */
data class SharedContent(val text: String, val uris: List<Uri>) {
  val isEmpty: Boolean get() = text.isBlank() && uris.isEmpty()
}

/** The shared content the composer of `conversation` picks up once (set by the share picker). */
object PendingShare {
  data class Target(val conversation: String, val content: SharedContent)

  val current = MutableStateFlow<Target?>(null)
}

/**
 * What an `ACTION_SEND` / `ACTION_SEND_MULTIPLE` intent shares, or nothing. Only `content:` URIs
 * of other apps are taken: a `file:` URI or one of the app's own FileProvider could point into the
 * app's private files, and sending those would leak them.
 */
fun sharedContent(intent: Intent, packageName: String): SharedContent? {
  val uris = when (intent.action) {
    Intent.ACTION_SEND -> listOfNotNull(intent.streamExtra())
    Intent.ACTION_SEND_MULTIPLE -> intent.streamListExtra()
    else -> return null
  }
  val content = SharedContent(
    text = intent.getStringExtra(Intent.EXTRA_TEXT).orEmpty().trim(),
    uris = uris.filter { isForeignContent(it.scheme, it.authority, packageName) }.take(MAX_SHARED_FILES),
  )
  return content.takeUnless { it.isEmpty }
}

/** A `content:` URI that another app serves (not this app's own provider). */
fun isForeignContent(maybeScheme: String?, maybeAuthority: String?, packageName: String): Boolean {
  val authority = maybeAuthority ?: return false
  val isOwn = authority == packageName || authority.startsWith("$packageName.")
  return maybeScheme.equals("content", ignoreCase = true) && !isOwn
}

private fun Intent.streamExtra(): Uri? =
  if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
    getParcelableExtra(Intent.EXTRA_STREAM, Uri::class.java)
  } else {
    @Suppress("DEPRECATION")
    getParcelableExtra(Intent.EXTRA_STREAM)
  }

private fun Intent.streamListExtra(): List<Uri> =
  if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
    getParcelableArrayListExtra(Intent.EXTRA_STREAM, Uri::class.java).orEmpty()
  } else {
    @Suppress("DEPRECATION")
    getParcelableArrayListExtra<Uri>(Intent.EXTRA_STREAM).orEmpty()
  }

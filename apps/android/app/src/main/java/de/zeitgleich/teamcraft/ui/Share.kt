package de.zeitgleich.teamcraft.ui

import android.content.Intent
import android.net.Uri
import android.os.Build
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow

/** Most files one share brings into the composer; more is almost certainly a mistake. */
const val MAX_SHARED_FILES = 10

/** Longest shared text taken into the composer; the rest is dropped. */
const val MAX_SHARED_TEXT_CHARS = 64 * 1024

/**
 * What an intent brought, until it is copied into the [ShareInbox]. [maybeTarget] is the conversation
 * the share sheet's direct share shortcut picked ([ShareShortcuts]); [leftOut] counts the URIs not
 * taken (over [MAX_SHARED_FILES], or into this app's own files).
 */
data class SharedContent(
  val text: String,
  val uris: List<Uri>,
  val maybeTarget: String? = null,
  val leftOut: Int = 0,
) {
  val isEmpty: Boolean get() = text.isBlank() && uris.isEmpty()
}

/** Where a share stands: its files still being copied, or waiting for a conversation. */
sealed interface ShareState {
  data object Preparing : ShareState
  data class Ready(val share: StagedShare) : ShareState
}

/** The shared content the composer of a conversation picks up once (set by the share picker). */
object PendingShare : ShareHandOff()

/**
 * Hands a picked share to one conversation's composer, which takes it exactly once. A composer can
 * be composed twice (a screen recreated during a transition, a repeated pick), and each copy would
 * otherwise append the share to the draft the other one already saved.
 */
open class ShareHandOff {
  data class Target(val conversation: String, val content: StagedShare)

  private val target = MutableStateFlow<Target?>(null)

  /** Shares offered so far; the same share offered again (a repeated pick) is refused. */
  private val offered = ArrayDeque<StagedShare>()

  val current: StateFlow<Target?> = target

  /**
   * Offers [content] to the composer of [conversation]; false when this very share was offered
   * before. A share whose files failed comes back changed (without its text), and may be offered.
   */
  @Synchronized
  fun offer(conversation: String, content: StagedShare): Boolean {
    if (content in offered) return false
    offered.addLast(content)
    while (offered.size > MAX_REMEMBERED_OFFERS) offered.removeFirst()
    target.value = Target(conversation, content)
    return true
  }

  /** Takes the share waiting for [conversation]: the first caller gets it, every later one nothing. */
  fun claim(conversation: String): StagedShare? {
    while (true) {
      val waiting = target.value?.takeIf { it.conversation == conversation } ?: return null
      if (target.compareAndSet(waiting, null)) return waiting.content
    }
  }

  /** Nothing waits any more (the session ended). */
  @Synchronized
  fun clear() {
    target.value = null
    offered.clear()
  }

  private companion object {
    /** Offers remembered to refuse a repeat; a repeat follows its first offer within seconds. */
    const val MAX_REMEMBERED_OFFERS = 16
  }
}

/**
 * What an `ACTION_SEND` / `ACTION_SEND_MULTIPLE` intent shares, or nothing. Only `content:` URIs
 * of other apps are taken: a `file:` URI or one of the app's own FileProvider could point into the
 * app's private files, and sending those would leak them.
 */
fun sharedContent(intent: Intent, packageName: String): SharedContent? {
  val uris = when (intent.action) {
    Intent.ACTION_SEND -> listOfNotNull(runCatching { intent.streamExtra() }.getOrNull())
    Intent.ACTION_SEND_MULTIPLE -> runCatching { intent.streamListExtra() }.getOrDefault(emptyList())
    else -> return null
  }
  // A hostile sender can put anything into the extras; a wrong type must not crash the app.
  val text = runCatching { intent.getStringExtra(Intent.EXTRA_TEXT) }.getOrNull().orEmpty()
  val maybeSubject = runCatching { intent.getStringExtra(Intent.EXTRA_SUBJECT) }.getOrNull()
  val taken = uris.filter { isForeignContent(it.scheme, it.authority, packageName) }.take(MAX_SHARED_FILES)
  val content = SharedContent(
    text = sharedText(maybeSubject, text),
    uris = taken,
    maybeTarget = ShareShortcuts.targetOf(intent),
    leftOut = uris.size - taken.size,
  )
  return content.takeUnless { it.isEmpty && it.leftOut == 0 }
}

/**
 * The text of a share. Browsers send a page's title as the subject and only its address as the text;
 * the title then goes on top. A subject next to other text (a mail's) stays out.
 */
fun sharedText(maybeSubject: String?, text: String): String {
  val body = text.take(MAX_SHARED_TEXT_CHARS).trim()
  val subject = maybeSubject.orEmpty().trim().take(MAX_SHARED_TEXT_CHARS)
  val isOnlyAddress = !body.contains(Regex("\\s")) && (body.startsWith("https://") || body.startsWith("http://"))
  return if (isOnlyAddress && subject.isNotEmpty() && subject != body) "$subject\n$body" else body
}

/**
 * A `content:` URI that another app serves (not this app's own provider). Authorities with a user
 * prefix (`0@authority`) are refused: the resolver strips the prefix, so `0@<package>.files` would
 * reach the app's own provider while looking foreign.
 */
fun isForeignContent(maybeScheme: String?, maybeAuthority: String?, packageName: String): Boolean {
  val authority = maybeAuthority ?: return false
  if (authority.contains('@')) return false
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

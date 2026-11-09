package de.zeitgleich.teamcraft.core

import java.security.MessageDigest
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.update

/**
 * Where each composer's send stands (#1579). A send is never silent: the composer keeps its text and
 * files while the core takes the message ([ComposerSendState.Sending]), and gets them back with the
 * reason when it didn't ([ComposerSendState.Failed]), with "Try again". Only once the core has the
 * message (it is in the outbox, its files go up from there and survive a restart) does the composer
 * clear.
 *
 * Each attempt carries a client message ID. A retry of the same content takes the same ID, also
 * after the app was killed mid-send ([SendAttemptStore]), so the core keeps one message: it drops a
 * send whose ID it has stored already.
 */
class ComposerSend(
  private val store: SendAttemptStore,
  private val newId: () -> String = DraftCopy::newId,
) {
  private val states = MutableStateFlow<Map<String, ComposerSendState>>(emptyMap())

  /** Every composer's state, by composer key; a composer not in it is [ComposerSendState.Idle]. */
  val all: StateFlow<Map<String, ComposerSendState>> = states

  fun state(key: String): ComposerSendState = states.value[key] ?: ComposerSendState.Idle

  /**
   * Starts sending [content] from the composer [key]; `null` while one of its sends runs (a double
   * tap sends once). Same content as an attempt that didn't finish: that attempt's ID again.
   */
  fun start(key: String, content: SendContent): SendAttempt? {
    val current = state(key)
    if (current is ComposerSendState.Sending) return null
    val fingerprint = content.fingerprint()
    val maybePrevious = (current as? ComposerSendState.Failed)?.attempt ?: store.load(key)
    val attempt = maybePrevious?.takeIf { it.fingerprint == fingerprint } ?: SendAttempt(newId(), fingerprint)
    // Kept before the core hears of it: a kill right after leaves the ID for the retry.
    store.save(key, attempt)
    set(key, ComposerSendState.Sending(attempt))
    return attempt
  }

  /** The core has the message: the composer clears, nothing waits for a retry. */
  fun succeeded(key: String, attempt: SendAttempt) {
    if (store.load(key) == attempt) store.clear(key)
    if ((state(key) as? ComposerSendState.Sending)?.attempt == attempt) set(key, ComposerSendState.Idle)
  }

  /** The core refused or never got it: the composer shows [reason] and keeps the attempt for a retry. */
  fun failed(key: String, attempt: SendAttempt, reason: Int) {
    if ((state(key) as? ComposerSendState.Sending)?.attempt == attempt) {
      set(key, ComposerSendState.Failed(attempt, reason))
    }
  }

  /**
   * The user closed the failure. The attempt's ID stays kept: the core may have the message after
   * all (an answer lost), and sending the same content again must not make it twice.
   */
  fun dismiss(key: String) {
    if (state(key) is ComposerSendState.Failed) set(key, ComposerSendState.Idle)
  }

  private fun set(key: String, state: ComposerSendState) {
    states.update { if (state == ComposerSendState.Idle) it - key else it + (key to state) }
  }
}

/** Where one composer's send stands. */
sealed interface ComposerSendState {
  data object Idle : ComposerSendState

  data class Sending(val attempt: SendAttempt) : ComposerSendState

  /** [reason] is the failure's string resource, worded by the core's error. */
  data class Failed(val attempt: SendAttempt, val reason: Int) : ComposerSendState
}

/** One send: its client message ID and what it sent ([SendContent.fingerprint], no plaintext). */
data class SendAttempt(val clientMessageId: String, val fingerprint: String)

/** What a composer sends; equal content is the same message, so a retry keeps its ID. */
data class SendContent(val text: String, val filePaths: List<String>, val draftFileIds: List<String>) {
  /** A digest of the content: kept on the device for the retry without the message's text. */
  fun fingerprint(): String {
    val digest = MessageDigest.getInstance("SHA-256")
    for (part in listOf(text) + filePaths + listOf("\u0000") + draftFileIds) {
      digest.update(part.toByteArray())
      digest.update(0)
    }
    return digest.digest().joinToString("") { "%02x".format(it) }
  }
}

/** Keeps the unconfirmed attempt of each composer across a restart. */
interface SendAttemptStore {
  fun load(key: String): SendAttempt?

  fun save(key: String, attempt: SendAttempt)

  fun clear(key: String)
}

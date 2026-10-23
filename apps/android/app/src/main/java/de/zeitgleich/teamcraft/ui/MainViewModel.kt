package de.zeitgleich.teamcraft.ui

import android.app.Application
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.provider.OpenableColumns
import java.io.File
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import androidx.annotation.StringRes
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.TeamCraftApplication
import de.zeitgleich.teamcraft.calls.CallOptions
import de.zeitgleich.teamcraft.core.ChatClient
import de.zeitgleich.teamcraft.core.PickedFile
import uniffi.teamcraft_ffi.AccountState
import uniffi.teamcraft_ffi.ConversationItem
import uniffi.teamcraft_ffi.ConversationKind
import uniffi.teamcraft_ffi.MessageItem
import uniffi.teamcraft_ffi.Presence
import uniffi.teamcraft_ffi.PushService
import uniffi.teamcraft_ffi.SearchHitItem
import uniffi.teamcraft_ffi.TeamCraftException

/** What the app shows at the top level. */
sealed interface AppState {
  data object Starting : AppState
  data class SignedOut(@StringRes val maybeError: Int? = null) : AppState
  data class NeedsEnrollment(val hasExistingIdentity: Boolean, @StringRes val maybeError: Int? = null) : AppState
  data class LinkingThisDevice(val code: String, val maybeShortCode: String? = null) : AppState
  data class RecoveringThroughOrganization(val code: String, @StringRes val maybeError: Int? = null) : AppState
  data class Ready(val userId: String, val displayName: String, val identityFingerprint: String) : AppState
  data class Failed(@StringRes val error: Int) : AppState
}

class MainViewModel(application: Application) : AndroidViewModel(application) {
  private val graph = TeamCraftApplication.graph(application)
  private val state = MutableStateFlow<AppState>(AppState.Starting)
  private val busy = MutableStateFlow(false)

  val appState: StateFlow<AppState> = state
  val isBusy: StateFlow<Boolean> = busy
  /** Increments on every store change; screens re-read when it changes. */
  val changeCount: StateFlow<Long> = graph.core.changeCount

  /** The chat features over the core's JSON bridge. */
  val chat = ChatClient { graph.core.core() }
  private val failures = MutableStateFlow<Int?>(null)

  /** The last action that failed, as a message to show once. */
  val lastFailure: StateFlow<Int?> = failures

  init {
    refresh()
  }

  fun refresh() = launchBusy {
    state.value = if (!graph.auth.isSignedIn.value) AppState.SignedOut() else loadAccountState()
  }

  suspend fun loginIntent(): Intent = graph.auth.loginIntent()

  fun completeLogin(result: Intent?) = launchBusy {
    state.value = try {
      graph.auth.completeLogin(result)
      loadAccountState()
    } catch (failure: Exception) {
      AppState.SignedOut(R.string.sign_in_failed)
    }
  }

  fun enroll(passphrase: String) = launchBusy {
    val current = state.value as? AppState.NeedsEnrollment ?: return@launchBusy
    state.value = try {
      ready(graph.core.core().enroll(deviceName(), passphrase))
    } catch (failure: TeamCraftException) {
      current.copy(maybeError = errorText(failure))
    }
  }

  fun linkThisDevice() = launchBusy {
    val core = graph.core.core()
    val code = core.startDeviceLink()
    state.value = AppState.LinkingThisDevice(code)
    try {
      val shortCode = core.waitForLinkPeer()
      state.value = AppState.LinkingThisDevice(code, shortCode)
      state.value = ready(core.finishDeviceLink(deviceName()))
    } catch (failure: TeamCraftException) {
      state.value = AppState.NeedsEnrollment(hasExistingIdentity = true, maybeError = errorText(failure))
    }
  }

  /** Asks the organization's admins to recover the account; shows the code to read to one. */
  fun recoverThroughOrganization() = launchBusy {
    state.value = try {
      AppState.RecoveringThroughOrganization(graph.core.core().startOrgRecovery().verificationCode)
    } catch (failure: TeamCraftException) {
      AppState.NeedsEnrollment(hasExistingIdentity = true, maybeError = errorText(failure))
    }
  }

  /** After the admins approved: enrolls with the recovered keys and the new passphrase. */
  fun finishOrgRecovery(newPassphrase: String) = launchBusy {
    val current = state.value as? AppState.RecoveringThroughOrganization ?: return@launchBusy
    state.value = try {
      ready(graph.core.core().finishOrgRecovery(deviceName(), newPassphrase))
    } catch (failure: TeamCraftException.RecoveryPending) {
      current.copy(maybeError = R.string.org_recovery_pending)
    } catch (failure: TeamCraftException) {
      current.copy(maybeError = errorText(failure))
    }
  }

  fun cancelLinking() {
    state.value = AppState.NeedsEnrollment(hasExistingIdentity = true)
  }

  /**
   * Signs out and removes what the account left in the clear on this device: notifications,
   * decrypted downloads and staged uploads, and the push registration. The sealed store stays.
   */
  fun signOut() = launchBusy {
    // Wake-ups that arrive from now on show nothing; notifications are cleared last, when no
    // core can be opened any more to post new ones.
    graph.push.beginSignOut()
    graph.calls.hangUp()
    graph.push.unregister()
    runCatching { graph.core.core().deletePlaintextFiles() }
    graph.core.close()
    graph.auth.signOut()
    graph.notifier.clearAll()
    state.value = AppState.SignedOut()
  }

  suspend fun conversations(): List<ConversationItem> = read { conversations() } ?: emptyList()

  /**
   * The newest message of each conversation, for the chat list. Reads the local store only (one
   * small window per conversation); names come from the snapshot.
   */
  suspend fun chatPreviews(conversations: List<ConversationItem>): Map<String, ChatListEntry> {
    val snapshot = readChat { snapshot() } ?: return emptyMap()
    return conversations.mapNotNull { conversation ->
      val latest = readChat { latest(conversation.id, 1).messages.lastOrNull() } ?: return@mapNotNull null
      val preview = chatPreview(
        message = latest,
        myId = snapshot.me.id,
        authorName = snapshot.name(latest.author),
        isOneToOne = conversation.kind == ConversationKind.DIRECT,
      )
      conversation.id to ChatListEntry(preview, latest.id)
    }.toMap()
  }

  suspend fun messages(conversationId: String): List<MessageItem> =
    read { messages(conversationId, MESSAGE_PAGE.toUInt()) } ?: emptyList()

  suspend fun thread(conversationId: String, threadId: String): List<MessageItem> =
    read { thread(conversationId, threadId) } ?: emptyList()

  suspend fun search(query: String): List<SearchHitItem> =
    if (query.isBlank()) emptyList() else read { search(query) } ?: emptyList()

  fun send(conversationId: String, maybeThreadId: String?, body: String) = viewModelScope.launch {
    runCatching { graph.core.core().sendMessage(conversationId, maybeThreadId, body) }
  }

  fun markRead(conversationId: String) = viewModelScope.launch {
    runCatching { graph.core.core().markRead(conversationId) }
    graph.notifier.dismiss(conversationId)
  }

  fun setPresence(presence: Presence) = viewModelScope.launch {
    runCatching { graph.core.core().setPresence(presence) }
  }

  /** Runs a `ChatAction` built with [ChatActions]; failures show as [lastFailure]. */
  fun act(action: String) = viewModelScope.launch {
    try {
      chat.act(action)
    } catch (failure: TeamCraftException) {
      failures.value = errorText(failure)
    }
  }

  fun failureShown() {
    failures.value = null
  }

  /** Runs a read; `null` when the core can't answer (not enrolled, gone). */
  suspend fun <T> readChat(block: suspend ChatClient.() -> T): T? =
    try {
      chat.block()
    } catch (failure: TeamCraftException) {
      null
    } catch (failure: kotlinx.serialization.SerializationException) {
      null
    }

  /**
   * Deletes the staged copy of a file taken out of the composer before sending. Only files in the
   * account's staging folder; anything else is not the app's to delete.
   */
  fun discardFile(file: PickedFile) = viewModelScope.launch(Dispatchers.IO) {
    val directory = runCatching { File(graph.core.core().uploadStagingDir()).canonicalFile }.getOrNull()
      ?: return@launch
    val staged = File(file.path).canonicalFile
    if (staged.parentFile == directory) staged.delete()
  }

  /** Copies a picked document into the account's staging folder so the core can read and encrypt it. */
  suspend fun importFile(uri: Uri): PickedFile? = withContext(Dispatchers.IO) {
    val resolver = getApplication<Application>().contentResolver
    val name = resolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { cursor ->
      if (cursor.moveToFirst()) cursor.getString(0) else null
    } ?: "file"
    val type = resolver.getType(uri) ?: "application/octet-stream"
    // The core deletes the copy once it is uploaded (or failed for good).
    val directory = runCatching { File(graph.core.core().uploadStagingDir()) }.getOrNull() ?: return@withContext null
    val target = File(directory, "${System.nanoTime()}-${name.replace('/', '_')}")
    runCatching {
      resolver.openInputStream(uri)?.use { input -> target.outputStream().use { input.copyTo(it) } }
        ?: return@withContext null
      PickedFile(fileName = name, mediaType = type, path = target.absolutePath)
    }.getOrNull()
  }

  /** Joins (or starts) a conversation's call, e.g. a meeting's. */
  fun startCall(conversationId: String, displayName: String, options: CallOptions = CallOptions()) =
    graph.calls.startCall(conversationId, displayName, options)

  /** Ask before `@channel` in conversations with more members than this (0 = never). */
  fun confirmChannelOver(): Int = graph.mentionPrefs.confirmChannelOver

  val mentionPrefs get() = graph.mentionPrefs

  val segmentPrefetch get() = graph.segmentPrefetch

  fun pushService(): PushService? = graph.push.activeService()

  fun registerPush() = graph.push.register()

  /** Existing device: starts approving a new device; returns the short code to compare. */
  suspend fun approveLink(code: String): Result<String> =
    runCatching { graph.core.core().approveDeviceLink(code.trim()) }

  suspend fun confirmLink(): Result<Unit> = runCatching { graph.core.core().confirmDeviceLink() }

  /** The user left the code comparison: nothing is sent, syncing resumes. */
  fun cancelLinkApproval() {
    graph.scope.launch { runCatching { graph.core.core().cancelDeviceLinkApproval() } }
  }

  private suspend fun loadAccountState(): AppState =
    try {
      when (val account = graph.core.core().accountState()) {
        is AccountState.NeedsEnrollment -> AppState.NeedsEnrollment(account.hasExistingIdentity)
        is AccountState.Ready -> ready(account)
      }
    } catch (failure: TeamCraftException.SignedOut) {
      graph.auth.signOut()
      AppState.SignedOut(R.string.error_signed_out)
    } catch (failure: TeamCraftException) {
      AppState.Failed(errorText(failure))
    }

  private suspend fun ready(account: AccountState): AppState {
    val ready = account as? AccountState.Ready ?: return AppState.NeedsEnrollment(hasExistingIdentity = true)
    runCatching { graph.core.core().start() }
    graph.push.register()
    return AppState.Ready(ready.userId, ready.displayName, ready.identityFingerprint)
  }

  private suspend fun <T> read(block: suspend uniffi.teamcraft_ffi.TeamCraft.() -> T): T? =
    runCatching { graph.core.core().block() }.getOrNull()

  private fun launchBusy(block: suspend () -> Unit) = viewModelScope.launch {
    busy.value = true
    try {
      block()
    } finally {
      busy.value = false
    }
  }

  private fun deviceName(): String =
    "${getApplication<Application>().getString(R.string.device_name)} (${Build.MODEL})"

  companion object {
    const val MESSAGE_PAGE = 200

    @StringRes
    fun errorText(failure: TeamCraftException): Int = when (failure) {
      is TeamCraftException.Network -> R.string.error_network
      is TeamCraftException.SignedOut -> R.string.error_signed_out
      is TeamCraftException.WrongPassphrase -> R.string.error_wrong_passphrase
      is TeamCraftException.Storage -> R.string.error_storage
      else -> R.string.error_generic
    }
  }
}

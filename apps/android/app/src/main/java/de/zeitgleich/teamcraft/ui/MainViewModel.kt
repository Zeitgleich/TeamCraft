package de.zeitgleich.teamcraft.ui

import android.app.Application
import android.content.Intent
import android.graphics.Bitmap
import android.net.Uri
import android.os.Build
import android.provider.OpenableColumns
import android.system.Os
import java.io.File
import java.io.IOException
import java.io.InputStream
import java.io.OutputStream
import java.util.Locale
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import androidx.annotation.StringRes
import androidx.compose.runtime.mutableStateMapOf
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch
import de.zeitgleich.teamcraft.BuildConfig
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.TeamCraftApplication
import de.zeitgleich.teamcraft.calls.CallOptions
import de.zeitgleich.teamcraft.core.ChatClient
import de.zeitgleich.teamcraft.core.ChatQueries
import de.zeitgleich.teamcraft.core.PermalinkTarget
import de.zeitgleich.teamcraft.core.PickedFile
import de.zeitgleich.teamcraft.push.UnifiedPushBackend
import de.zeitgleich.teamcraft.push.UnifiedPushStatus
import de.zeitgleich.teamcraft.server.ServerInfoAnswer
import de.zeitgleich.teamcraft.server.ServerInfoClient
import de.zeitgleich.teamcraft.server.ServerLock
import de.zeitgleich.teamcraft.server.ServerStart
import de.zeitgleich.teamcraft.server.decideServerStart
import de.zeitgleich.teamcraft.server.isRetryableWhenOnline
import de.zeitgleich.teamcraft.server.isServerAllowed
import de.zeitgleich.teamcraft.server.serverEntryErrorText
import de.zeitgleich.teamcraft.server.transportErrorText
import net.openid.appauth.AuthorizationException
import uniffi.teamcraft_ffi.AccountState
import uniffi.teamcraft_ffi.CheckedServer
import uniffi.teamcraft_ffi.ConversationItem
import uniffi.teamcraft_ffi.ConversationKind
import uniffi.teamcraft_ffi.LoginBrand
import uniffi.teamcraft_ffi.MessageItem
import uniffi.teamcraft_ffi.Presence
import uniffi.teamcraft_ffi.PushDiagnosticsInfo
import uniffi.teamcraft_ffi.PushService
import uniffi.teamcraft_ffi.SearchHitItem
import uniffi.teamcraft_ffi.ServerEntryException
import uniffi.teamcraft_ffi.StoredServer
import uniffi.teamcraft_ffi.checkServerInfo
import uniffi.teamcraft_ffi.normalizeServerAddress
import uniffi.teamcraft_ffi.TeamCraftException
import uniffi.teamcraft_ffi.TestPushResult

/** What the app shows at the top level. */
sealed interface AppState {
  data object Starting : AppState

  /**
   * No server yet, or the user wants another: ask for its address. [prefill] is what was tried
   * last (or the managed server); with [isLocked] the organization fixed the server.
   */
  data class NeedsServer(val prefill: String, @StringRes val maybeError: Int? = null, val isLocked: Boolean = false) : AppState

  /** The server answered and passed the checks: show who it is before signing in. */
  data class ConfirmServer(val checked: CheckedServer) : AppState
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

  /** Files picked in a composer and not sent yet, by conversation and thread. */
  val pickedFiles = mutableStateMapOf<String, List<PickedFile>>()

  /** The last action that failed, as a message to show once. */
  val lastFailure: StateFlow<Int?> = failures

  @Volatile private var maybeBrandStore: BrandStore? = null
  private val brand = MutableStateFlow<LoginBrand?>(null)

  /** The server's brand for the sign-in screen; the cached one first, then the server's answer. */
  val loginBrand: StateFlow<LoginBrand?> = brand

  private val activeServer = MutableStateFlow<StoredServer?>(null)

  /** The server the app signs in to; the sign-in screen names its host. */
  val server: StateFlow<StoredServer?> = activeServer

  private var maybeLock: ServerLock? = null
  private val serverLocked = MutableStateFlow(false)

  /** The organization fixed the server (managed configuration): no "Change server". */
  val isServerLocked: StateFlow<Boolean> = serverLocked

  init {
    refresh()
    retryServerWhenOnline()
  }

  fun brandLogo(isDark: Boolean): Bitmap? = maybeBrandStore?.logo(isDark)

  /**
   * Decides where the app starts: the stored server, the managed (or build default) server to
   * confirm, or the server entry. The managed configuration is read again every time.
   */
  fun refresh() = launchBusy {
    val maybePreset = withContext(Dispatchers.IO) { graph.managedConfig.preset() }
    val lock = maybePreset?.takeIf { it.isLocked }?.let {
      ServerLock(it.address, withContext(Dispatchers.Default) { normalizedOrigin(it.address) })
    }
    maybeLock = lock
    serverLocked.value = lock != null
    // The first read opens the stored server and session: files and the core's checks.
    val maybeStored = withContext(Dispatchers.IO) { graph.auth.server }
    val start = decideServerStart(maybeStored?.address?.origin, maybePreset, lock)
    when {
      maybeStored != null && start is ServerStart.UseStored -> {
        showServer(maybeStored)
        state.value = if (!graph.auth.isSignedIn.value) AppState.SignedOut() else loadAccountState()
      }
      maybeStored != null && start is ServerStart.ReplaceStored -> {
        // The organization locked another server: this account can't stay on the device.
        endSession()
        graph.servers.forget(maybeStored.address.origin)
        forgetActiveServer()
        checkServer(start.address)
      }
      start is ServerStart.CheckPreset -> checkServer(start.preset.address)
      else -> state.value = AppState.NeedsServer(prefill = "")
    }
  }

  /** The user entered (or pasted) a server address. */
  fun submitServer(input: String) = launchBusy { checkServer(input) }

  /**
   * The user confirmed the server: it becomes the account's. True when the browser sign-in
   * should follow; false when a session for it exists already (or nothing was to confirm).
   */
  suspend fun confirmServer(): Boolean {
    val current = state.value as? AppState.ConfirmServer ?: return false
    val server = current.checked.server
    graph.servers.save(server)
    withContext(Dispatchers.IO) { graph.auth.activate(server) }
    showServer(server)
    if (!graph.auth.isSignedIn.value) {
      state.value = AppState.SignedOut()
      return true
    }
    launchBusy { state.value = loadAccountState() }
    return false
  }

  /** From the confirmation: back to the entry for a different address. */
  fun useAnotherServer() {
    if (state.value !is AppState.ConfirmServer || maybeLock != null) return
    state.value = AppState.NeedsServer(prefill = "")
  }

  /**
   * From the sign-in screen: forget the server's profile and enter another one. The account's
   * local data stays, keyed by the server, for when the user comes back to it.
   */
  fun changeServer() = launchBusy {
    if (maybeLock != null || graph.auth.isSignedIn.value) return@launchBusy
    graph.auth.server?.let { graph.servers.forget(it.address.origin) }
    graph.core.close()
    forgetActiveServer()
    state.value = AppState.NeedsServer(prefill = "")
  }

  /**
   * The browser intent for signing in; null (with the reason on the sign-in screen) when the
   * identity provider can't be reached.
   */
  suspend fun loginIntent(): Intent? =
    try {
      graph.auth.loginIntent()
    } catch (failure: Exception) {
      state.value = AppState.SignedOut(signInError(failure) ?: R.string.sign_in_failed)
      null
    }

  fun completeLogin(result: Intent?) = launchBusy {
    state.value = try {
      graph.auth.completeLogin(result)
      loadAccountState()
    } catch (failure: Exception) {
      AppState.SignedOut(signInError(failure))
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
    endSession()
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

  /** Shows a message once, like a failed action. */
  fun showFailure(@StringRes message: Int) {
    failures.value = message
  }

  /** Where a message link points, if this device is in that conversation and has the message. */
  suspend fun resolvePermalink(link: String): PermalinkTarget? =
    readChat { query(ChatQueries.resolvePermalink(link)).maybeTarget }

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
    val app = getApplication<Application>()
    val resolver = app.contentResolver
    // Picker and share URIs come from other apps; none may point into the app's private files.
    if (!isForeignContent(uri.scheme, uri.authority, app.packageName) || isPrivateFile(uri)) {
      return@withContext null
    }
    val name = runCatching {
      resolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { cursor ->
        if (cursor.moveToFirst()) cursor.getString(0) else null
      }
    }.getOrNull()?.let(::cleanFileName)?.takeIf { it.isNotBlank() } ?: "file"
    val type = runCatching { resolver.getType(uri) }.getOrNull() ?: "application/octet-stream"
    stage(name, type) { resolver.openInputStream(uri) }
  }

  /** A photo the camera wrote into the app's cache. */
  suspend fun importPhoto(photo: File): PickedFile? = withContext(Dispatchers.IO) {
    stage(photo.name, "image/jpeg") { photo.inputStream() }
  }

  /** Copies into the core's staging folder, at most [MAX_IMPORT_BYTES]. */
  private suspend fun stage(name: String, type: String, open: () -> InputStream?): PickedFile? {
    // The core deletes the copy once it is uploaded (or failed for good).
    val directory = runCatching { File(graph.core.core().uploadStagingDir()) }.getOrNull() ?: return null
    val target = File(directory, "${System.nanoTime()}-${name.replace('/', '_')}")
    val isCopied = runCatching {
      open()?.use { input -> target.outputStream().use { copyAtMost(input, it, MAX_IMPORT_BYTES) } } ?: false
    }.getOrDefault(false)
    if (!isCopied) {
      target.delete()
      return null
    }
    return PickedFile(fileName = name, mediaType = type, path = target.absolutePath)
  }

  /**
   * The provider handed out a descriptor of a file inside the app's own data (a provider of another
   * app can't, but a misconfigured or spoofed own one could). Pipes and other apps' files pass.
   */
  private fun isPrivateFile(uri: Uri): Boolean {
    val app = getApplication<Application>()
    val maybePath = runCatching {
      app.contentResolver.openFileDescriptor(uri, "r")?.use { Os.readlink("/proc/self/fd/${it.fd}") }
    }.getOrNull() ?: return false
    return maybePath.contains("/${app.packageName}/")
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

  /** What this device saw of pushes; `null` when the core can't answer. */
  suspend fun pushDiagnostics(): PushDiagnosticsInfo? = read { pushDiagnostics() }

  /** Asks the server for a test push to this device. */
  suspend fun sendTestPush(): Result<TestPushResult> = runCatching { graph.core.core().sendTestPush() }

  /** Increments with every wake-up handled. */
  val pushHandledCount: StateFlow<Long> get() = graph.pushHandler.handledCount

  val pushStats get() = graph.pushStats

  fun unifiedPushStatus(): UnifiedPushStatus = UnifiedPushStatus(getApplication())

  fun selectDistributor(packageName: String) = UnifiedPushBackend.selectDistributor(getApplication(), packageName)

  /** Existing device: starts approving a new device; returns the short code to compare. */
  suspend fun approveLink(code: String): Result<String> =
    runCatching { graph.core.core().approveDeviceLink(code.trim()) }

  suspend fun confirmLink(): Result<Unit> = runCatching { graph.core.core().confirmDeviceLink() }

  /** The user left the code comparison: nothing is sent, syncing resumes. */
  fun cancelLinkApproval() {
    graph.scope.launch { runCatching { graph.core.core().cancelDeviceLinkApproval() } }
  }

  private suspend fun endSession() {
    // Wake-ups that arrive from now on show nothing; notifications are cleared last, when no
    // core can be opened any more to post new ones.
    graph.push.beginSignOut()
    graph.calls.hangUp()
    graph.push.unregister()
    runCatching { graph.core.core().deletePlaintextFiles() }
    graph.core.close()
    graph.auth.signOut()
    graph.notifier.clearAll()
    graph.pushStats.clear()
  }

  /**
   * Normalises [input], asks the server for its settings and checks them; shows the confirmation
   * or why it failed. A locked configuration refuses other servers before contacting them.
   */
  private suspend fun checkServer(input: String) {
    val isLocked = maybeLock != null
    state.value = try {
      val address = withContext(Dispatchers.Default) { normalizeServerAddress(input, ALLOW_INSECURE_HTTP) }
      if (!isServerAllowed(address.origin, maybeLock)) {
        AppState.NeedsServer(input, R.string.server_error_locked, isLocked)
      } else {
        val answer = ServerInfoClient.fetch(address.serverInfoUrl)
        val checked = withContext(Dispatchers.Default) { checkAnswer(address.origin, answer) }
        previewBrand(checked)
        AppState.ConfirmServer(checked)
      }
    } catch (failure: ServerEntryException) {
      AppState.NeedsServer(input, serverEntryErrorText(failure), isLocked)
    } catch (failure: IOException) {
      AppState.NeedsServer(input, transportErrorText(failure, graph.network.isOnline.value), isLocked)
    }
  }

  private fun checkAnswer(origin: String, answer: ServerInfoAnswer): CheckedServer =
    checkServerInfo(
      origin = origin,
      // HttpURLConnection reports -1 for an answer that isn't HTTP; the core rejects that as well.
      status = answer.status.coerceIn(0, UShort.MAX_VALUE.toInt()).toUShort(),
      contentType = answer.maybeContentType,
      body = answer.body,
      clientVersion = BuildConfig.VERSION_NAME,
      redirectUri = BuildConfig.OIDC_REDIRECT_URI,
      allowInsecureHttp = ALLOW_INSECURE_HTTP,
      language = Locale.getDefault().toLanguageTag(),
    )

  /** The managed server's origin for the lock; null when the configured address is invalid. */
  private fun normalizedOrigin(address: String): String? =
    try {
      normalizeServerAddress(address, ALLOW_INSECURE_HTTP).origin
    } catch (invalid: ServerEntryException) {
      null
    }

  /**
   * Offline or unreachable at the entry: try the same address again as soon as the device has a
   * validated connection, so the user doesn't have to notice and tap retry.
   */
  private fun retryServerWhenOnline() = viewModelScope.launch {
    graph.network.isOnline.collect { isOnline ->
      val current = state.value as? AppState.NeedsServer ?: return@collect
      val maybeError = current.maybeError
      if (isOnline && !busy.value && maybeError != null && isRetryableWhenOnline(maybeError)) {
        submitServer(current.prefill)
      }
    }
  }

  /** The confirmation screen shows the brand the server announced; logos follow when fetched. */
  private fun previewBrand(checked: CheckedServer) {
    val address = checked.server.address
    val store = BrandStore(getApplication(), address.origin, address.storageKey)
    maybeBrandStore = store
    brand.value = checked.maybeBrand
    viewModelScope.launch {
      val maybeFresh = store.refresh()
      if (maybeBrandStore === store) brand.value = maybeFresh ?: checked.maybeBrand
    }
  }

  /** The active server's brand: the cached one at once, then the server's answer. */
  private fun showServer(server: StoredServer) {
    activeServer.value = server
    val address = server.address
    if (maybeBrandStore?.let { it.origin == address.origin } != true) {
      maybeBrandStore = BrandStore(getApplication(), address.origin, address.storageKey)
      brand.value = null
    }
    val store = maybeBrandStore ?: return
    viewModelScope.launch {
      val maybeCached = withContext(Dispatchers.IO) { store.cached() }
      if (maybeBrandStore === store && maybeCached != null) brand.value = maybeCached
      val maybeFresh = store.refresh()
      if (maybeBrandStore === store) brand.value = maybeFresh
    }
  }

  private fun forgetActiveServer() {
    graph.auth.activate(null)
    activeServer.value = null
    maybeBrandStore = null
    brand.value = null
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

    /** Plain HTTP to other hosts than the device itself only in development builds. */
    private val ALLOW_INSECURE_HTTP = BuildConfig.DEBUG

    /** Why signing in failed, in words the user can act on; nothing when they cancelled it. */
    @StringRes
    fun signInError(failure: Exception): Int? = when {
      failure is AuthorizationException && failure.code == AuthorizationException.GeneralErrors.USER_CANCELED_AUTH_FLOW.code -> null
      failure is IllegalStateException && failure.message == "login cancelled" -> null
      failure is AuthorizationException && failure.code == AuthorizationException.GeneralErrors.NETWORK_ERROR.code ->
        R.string.sign_in_network
      failure is java.io.IOException -> R.string.sign_in_network
      failure is AuthorizationException && failure.type == AuthorizationException.TYPE_OAUTH_TOKEN_ERROR -> R.string.sign_in_rejected
      failure is TeamCraftException.Network -> R.string.sign_in_network
      else -> R.string.sign_in_failed
    }

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

/**
 * Largest file the app copies in for sending. The server enforces its own (smaller) limit; this
 * only keeps a hostile or endless provider from filling the device.
 */
private const val MAX_IMPORT_BYTES = 4L * 1024 * 1024 * 1024

/** Copies `input` to `output`; false (and stops) once more than `maxBytes` arrived. */
internal fun copyAtMost(input: InputStream, output: OutputStream, maxBytes: Long): Boolean {
  val buffer = ByteArray(64 * 1024)
  var total = 0L
  while (true) {
    val count = input.read(buffer)
    if (count < 0) return true
    total += count
    if (total > maxBytes) return false
    output.write(buffer, 0, count)
  }
}

/** A provider's display name without control or direction-override characters (spoofed extensions). */
internal fun cleanFileName(name: String): String =
  name.filterNot { it.isISOControl() || it in BIDI_CONTROLS }.trim().take(MAX_FILE_NAME_CHARS)

private const val MAX_FILE_NAME_CHARS = 255
private const val BIDI_CONTROLS = "\u061C\u200E\u200F\u202A\u202B\u202C\u202D\u202E\u2066\u2067\u2068\u2069"

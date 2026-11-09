package de.zeitgleich.teamcraft.ui

import de.zeitgleich.teamcraft.crash.Perf
import android.util.Log
import android.app.Application
import android.content.Intent
import android.graphics.Bitmap
import android.net.Uri
import android.os.Build
import android.os.SystemClock
import android.provider.OpenableColumns
import android.system.Os
import java.io.File
import java.io.IOException
import java.io.InputStream
import java.io.OutputStream
import java.util.Locale
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.withContext
import androidx.annotation.StringRes
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateMapOf
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.DefaultLifecycleObserver
import androidx.lifecycle.LifecycleOwner
import androidx.lifecycle.ProcessLifecycleOwner
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.flow.receiveAsFlow
import kotlinx.coroutines.flow.getAndUpdate
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.launch
import de.zeitgleich.teamcraft.BuildConfig
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.TeamCraftApplication
import de.zeitgleich.teamcraft.auth.LocalLoginOutcome
import de.zeitgleich.teamcraft.auth.SignInFailure
import de.zeitgleich.teamcraft.auth.providerRefusal
import de.zeitgleich.teamcraft.auth.signInFailure
import de.zeitgleich.teamcraft.auth.signInFailureLog
import de.zeitgleich.teamcraft.auth.isSsoOnly
import de.zeitgleich.teamcraft.auth.localSignInErrorText
import de.zeitgleich.teamcraft.calls.CallOptions
import de.zeitgleich.teamcraft.core.ChatActions
import de.zeitgleich.teamcraft.core.ComposerSend
import de.zeitgleich.teamcraft.core.ComposerSendState
import de.zeitgleich.teamcraft.core.SendAttempt
import de.zeitgleich.teamcraft.core.DraftFiles
import de.zeitgleich.teamcraft.ui.media.MediaDraftStore
import de.zeitgleich.teamcraft.ui.media.SealedCaptures
import de.zeitgleich.teamcraft.core.ChatClient
import de.zeitgleich.teamcraft.core.ChatQueries
import de.zeitgleich.teamcraft.core.FileInfo
import de.zeitgleich.teamcraft.core.HolidayRegion
import de.zeitgleich.teamcraft.core.NetworkTrust
import de.zeitgleich.teamcraft.core.OwnProfile
import de.zeitgleich.teamcraft.core.PermalinkTarget
import de.zeitgleich.teamcraft.core.PickedFile
import de.zeitgleich.teamcraft.core.TypingPlace
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
import uniffi.teamcraft_ffi.AccountState
import uniffi.teamcraft_ffi.AddPeopleOutcome
import uniffi.teamcraft_ffi.ChannelMembersInfo
import uniffi.teamcraft_ffi.ChannelRolesReviewItem
import uniffi.teamcraft_ffi.CheckedServer
import uniffi.teamcraft_ffi.ConversationItem
import uniffi.teamcraft_ffi.ConversationKind
import uniffi.teamcraft_ffi.CreateOutcome
import de.zeitgleich.teamcraft.ui.forms.CreateResult
import uniffi.teamcraft_ffi.LeaveChoice
import uniffi.teamcraft_ffi.LinkPlatform
import uniffi.teamcraft_ffi.LinkRequestItem
import uniffi.teamcraft_ffi.LoginBrand
import uniffi.teamcraft_ffi.MovedCallHint
import de.zeitgleich.teamcraft.core.LoadFailure
import uniffi.teamcraft_ffi.FirstSyncItem
import uniffi.teamcraft_ffi.KeyCatchUpItem
import uniffi.teamcraft_ffi.MessageItem
import uniffi.teamcraft_ffi.NotificationContent
import uniffi.teamcraft_ffi.NotifiedConversation
import uniffi.teamcraft_ffi.SpaceMenuItem
import uniffi.teamcraft_ffi.Presence
import uniffi.teamcraft_ffi.PushDiagnosticsInfo
import uniffi.teamcraft_ffi.PushService
import uniffi.teamcraft_ffi.ReviewedRolesOutcome
import uniffi.teamcraft_ffi.SearchHitItem
import uniffi.teamcraft_ffi.ServerEntryException
import uniffi.teamcraft_ffi.SignInPageInfo
import uniffi.teamcraft_ffi.StoreProblem
import uniffi.teamcraft_ffi.StoredServer
import uniffi.teamcraft_ffi.checkServerInfo
import uniffi.teamcraft_ffi.normalizeServerAddress
import uniffi.teamcraft_ffi.signInPage
import uniffi.teamcraft_ffi.ssoOnlySignInPage
import uniffi.teamcraft_ffi.TeamCraft
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
  /** [maybeErrorCode] is the identity provider's error code, for texts that name it. */
  data class SignedOut(@StringRes val maybeError: Int? = null, val maybeErrorCode: String? = null) : AppState
  data class NeedsEnrollment(val hasExistingIdentity: Boolean, @StringRes val maybeError: Int? = null) : AppState
  /**
   * This phone waits to be linked (issue #998): it shows no code, the other device approves it from a
   * prompt, or this phone scans the code a computer shows. [attempt] counts fresh starts, so the
   * screen forgets an earlier "asked"; [maybeNotice] says why the last attempt ended.
   */
  data class LinkingThisDevice(
    val maybeShortCode: String? = null,
    val attempt: Int = 0,
    @StringRes val maybeNotice: Int? = null,
  ) : AppState
  data class RecoveringThroughOrganization(val code: String, @StringRes val maybeError: Int? = null) : AppState
  data class Ready(val userId: String, val displayName: String, val identityFingerprint: String) : AppState
  data class Failed(@StringRes val error: Int) : AppState

  /** The local data can't be read by this version (issue #414); [maybeError] says why a reset failed. */
  data class StoreUnreadable(val problem: StoreProblem, @StringRes val maybeError: Int? = null) : AppState
}

/** The approval of a new device the user is in; the code to type is on the new device only. */
data class LinkReview(
  val deviceName: String?,
  @StringRes val maybeError: Int? = null,
  val isDone: Boolean = false,
  /** The typed code is being checked: the confirm button waits. */
  val isConfirming: Boolean = false,
)

/** What typing the new device's code did. */
enum class LinkConfirmation { CONFIRMED, WRONG_CODE, ATTEMPT_ENDED, FAILED }

class MainViewModel(application: Application) : AndroidViewModel(application) {
  private val graph = TeamCraftApplication.graph(application)
  private val state = MutableStateFlow<AppState>(AppState.Starting)
  // Above the `init` blocks: the first refresh fills it from the start cache before it suspends.
  private val chatListState = MutableStateFlow(ChatListState())
  private val chatListLock = Mutex()

  /**
   * The chat list's conversations and previews, kept here so that going back to the list shows it at
   * once instead of loading it again; the list refreshes it on every change while it is shown.
   */
  val chatList: StateFlow<ChatListState> = chatListState

  private val typingPlacesState = MutableStateFlow<List<TypingPlace>>(emptyList())
  private val typingNamesState = MutableStateFlow<Map<String, List<String>>>(emptyMap())

  /**
   * Everyone typing anywhere right now (#882), thread replies apart: a thread's line reads its own
   * place. Read on every change and, while someone types, every couple of seconds, since an
   * indicator runs out without an event.
   */
  val typingPlaces: StateFlow<List<TypingPlace>> = typingPlacesState

  /**
   * The names of those typing in each conversation itself (not in its threads): the chat list's
   * rows and the header read this. Equal maps don't emit, and a row compares only its own names.
   */
  val typingNames: StateFlow<Map<String, List<String>>> = typingNamesState
  private val startCache = graph.startCache
  private val feedSeeds = FeedSeeds()
  private var maybeReadAhead: Job? = null
  private var maybeStartCacheWrite: Job? = null

  /**
   * The store changed since the list was read last: a message deleted, redacted or purged while
   * a conversation was on screen would stay in the start cache. Read again when the app leaves.
   */
  @Volatile private var isStartCacheStale = false
  private val startCacheOnLeave = object : DefaultLifecycleObserver {
    override fun onStop(owner: LifecycleOwner) = refreshStartCacheIfStale()
  }
  private val busy = MutableStateFlow(false)
  private val shares = graph.shares
  private val shareState = MutableStateFlow<ShareState?>(null)
  private val shareOfOtherAccount = MutableStateFlow(false)

  /** A share couldn't be kept (full disk, too many waiting); the user has to be told. */
  val hasShareFailed: StateFlow<Boolean> = shares.hasFailed

  /** A share from before the sign-in went: it was kept for the person signed in before. */
  val hasShareGoneForAccount: StateFlow<Boolean> = shareOfOtherAccount

  /** What another app shared, until the user picked a conversation or discarded it (issue #208). */
  val share: StateFlow<ShareState?> = shareState

  /** A copy of [content] under way must start over if the process is killed: its intent isn't used up. */
  fun isCopyingShare(content: SharedContent): Boolean = shares.isCopying(content)

  fun shareFailureShown() {
    shares.failureShown()
  }

  fun shareGoneForAccountShown() {
    shareOfOtherAccount.value = false
  }

  val appState: StateFlow<AppState> = state
  val isBusy: StateFlow<Boolean> = busy
  /** Increments on every store change; screens re-read when it changes. */
  val changeCount: StateFlow<Long> = graph.core.changeCount

  /** The chat features over the core's JSON bridge. */
  val chat = ChatClient(
    ::deletedUserName,
    maybeChanges = { graph.core.changeCount.value },
    generation = { graph.core.accountGeneration.value },
  ) { graph.core.core() }
  private val failures = MutableStateFlow<Int?>(null)

  /** Files picked in a composer and not sent yet, by conversation and thread ([pickedKey]). */
  val pickedFiles = mutableStateMapOf<String, List<PickedFile>>()

  /**
   * Draft files the composers removed or sent (#1220): they don't show again while the core
   * catches up. IDs are random and few; the oldest go past [MAX_GONE_DRAFT_FILES].
   */
  val goneDraftFiles = mutableStateListOf<String>()

  private val sendAttempts = SendAttemptPrefs(application)

  /** Where each composer's send stands (#1579): pending, failed with a retry, or nothing. */
  val composerSend = ComposerSend(sendAttempts)

  /** Staged files whose draft copy is being made, so a second call doesn't attach them twice. */
  private val draftAttaching = mutableSetOf<String>()

  /** The app is in a call, or one rings: no voice recording then, and none plays (#204). */
  val isInCall: StateFlow<Boolean> = inCallFlow(graph.calls.callState, graph.calls.incomingRing, viewModelScope)

  /** The one voice recording of the app (#204). */
  val voice = VoiceRecorderModel(application, viewModelScope, isInCall) {
    runCatching { graph.core.uploadStagingDir() }.getOrNull()
  }

  /** The one player of voice messages (#204). */
  val voicePlayer = VoicePlayer(application, viewModelScope).also { player ->
    viewModelScope.launch { isInCall.collect { inCall -> if (inCall) player.pause() } }
  }

  /** The last action that failed, as a message to show once. */
  val lastFailure: StateFlow<Int?> = failures

  /** Admins' requests to join private channels: the owner's decisions and the admin's own (#799). */
  val joinRequests = JoinRequestActions(graph.core, viewModelScope) { failures.value = it }

  @Volatile private var maybeBrandStore: BrandStore? = null
  private val brand = MutableStateFlow<LoginBrand?>(null)

  /** The server's brand for the sign-in screen; the cached one first, then the server's answer. */
  val loginBrand: StateFlow<LoginBrand?> = brand

  private val icons = MutableStateFlow<BrandIcons?>(null)

  /** The brand's icons for its tile on the sign-in screen, decoded off the main thread. */
  val brandIcons: StateFlow<BrandIcons?> = icons

  private val activeServer = MutableStateFlow<StoredServer?>(null)

  /** The server the app signs in to; the sign-in screen names its host. */
  val server: StateFlow<StoredServer?> = activeServer

  private var maybeLock: ServerLock? = null
  private val serverLocked = MutableStateFlow(false)

  /** The organization fixed the server (managed configuration): no "Change server". */
  val isServerLocked: StateFlow<Boolean> = serverLocked

  /** The start read the stored server and the managed lock; the start cache may show the list before. */
  private val serverKnown = MutableStateFlow(false)

  /**
   * The server a link is judged against, in one value so a frame never sees a fresh "known" with a
   * stale server; nothing until the start read it.
   */
  val linkServer: StateFlow<LinkServer?> = combine(activeServer, serverLocked, serverKnown) { server, isLocked, isKnown ->
    linkServerOf(server?.address?.origin, isLocked, isKnown)
  }.stateIn(viewModelScope, SharingStarted.Eagerly, null)

  private val serverFromLink = MutableStateFlow(false)

  /** The server being confirmed came from a link, not from the user (docs/app-links.md). */
  val isServerFromLink: StateFlow<Boolean> = serverFromLink

  /** Whether the current sign-in already had its automatic retry; a user's new one resets it. */
  private var hasRetriedAuthorization = false
  private val loginRetries = Channel<Intent>(Channel.BUFFERED)

  /** Browser intents of the automatic sign-in retry, for the activity to open (issue #1063). */
  val loginRetryIntents: Flow<Intent> = loginRetries.receiveAsFlow()

  private val page = MutableStateFlow<SignInPageInfo?>(null)

  /**
   * What the sign-in screen offers, asked from the server every time the screen is shown (admins
   * change it at runtime); `null` until known, "Sign in with <brand>" only while offline.
   */
  val signInPage: StateFlow<SignInPageInfo?> = page

  private val pageIcon = MutableStateFlow<Bitmap?>(null)

  /** The identity provider's icon for "Sign in with …", cached per server. */
  val ssoIcon: StateFlow<Bitmap?> = pageIcon

  /** Which server [page] belongs to, whether it is the server's own answer, and when it came. */
  private var maybePageOrigin: String? = null
  private var isPageFresh = false
  private var pageFetchedAtMillis = 0L

  init {
    // What waited when the process ended comes back; what is past its time goes.
    viewModelScope.launch {
      withContext(Dispatchers.IO) { shares.inbox.prune(System.currentTimeMillis()) }
      // Also what a copy started by an activity that is gone leaves.
      shares.changes.collect { refreshShare() }
    }
  }

  init {
    // However the session ends (also an expired refresh token, which skips endSession), what it
    // showed leaves memory; the auth callback deletes what it left on disk.
    viewModelScope.launch {
      var wasSignedIn = false
      // The first read opens the stored session (a keystore decryption): not on the main thread.
      val signedIn = withContext(Dispatchers.IO) { graph.auth.isSignedIn }
      signedIn.collect { isSignedIn ->
        if (wasSignedIn && !isSignedIn) forgetSession()
        wasSignedIn = isSignedIn
      }
    }
    viewModelScope.launch(Dispatchers.IO) { MediaDraftStore.sweep(getApplication(), System.currentTimeMillis()) }
    viewModelScope.launch { graph.core.changeCount.collect { isStartCacheStale = true } }
    viewModelScope.launch {
      graph.core.changeCount.collectLatest {
        while (true) {
          refreshTyping()
          if (typingPlacesState.value.isEmpty()) break
          delay(TYPING_POLL_MS)
        }
      }
    }
    viewModelScope.launch(Dispatchers.IO) { graph.core.changeCount.collect { dropDraftsOfDroppedConversations() } }
    ProcessLifecycleOwner.get().lifecycle.addObserver(startCacheOnLeave)
    refresh()
    retryServerWhenOnline()
    loadSignInPageWhenShown()
    leaveWhenStoreTurnsUnreadable()
  }

  fun brandLogo(isDark: Boolean): Bitmap? = maybeBrandStore?.logo(isDark)

  /**
   * Decides where the app starts: the stored server, the managed (or build default) server to
   * confirm, or the server entry. The managed configuration is read again every time.
   */
  fun refresh() = launchBusy {
    val maybeShownAccount = if (state.value == AppState.Starting) showStartCache() else null
    val maybePreset = withContext(Dispatchers.IO) { graph.managedConfig.preset() }
    val lock = maybePreset?.takeIf { it.isLocked }?.let {
      ServerLock(it.address, withContext(Dispatchers.Default) { normalizedOrigin(it.address) })
    }
    maybeLock = lock
    serverLocked.value = lock != null
    // The first read opens the stored server and session: files and the core's checks.
    val maybeStored = withContext(Dispatchers.IO) { graph.auth.server }
    val start = decideServerStart(maybeStored?.address?.origin, maybePreset, lock)
    if (maybeStored != null && start is ServerStart.UseStored) showServer(maybeStored)
    serverKnown.value = true
    val isSignedIn = graph.auth.isSignedIn.value
    if (maybeShownAccount != null && (!isSignedIn || maybeShownAccount != accountKey())) {
      // Another account than the one shown last, or none: its list goes before anything else shows.
      chatListState.value = ChatListState()
      state.value = AppState.Starting
    }
    if (!isSignedIn || (maybeShownAccount != null && maybeShownAccount != accountKey())) {
      // A cache left by a sign-out that didn't finish (the app killed in between) goes too.
      withContext(Dispatchers.IO) { startCache.delete() }
    }
    when {
      maybeStored != null && start is ServerStart.UseStored -> {
        state.value = if (!graph.auth.isSignedIn.value) AppState.SignedOut() else loadAccountState()
      }
      maybeStored != null && start is ServerStart.ReplaceStored -> {
        // The organization locked another server: this account can't stay on the device.
        endSession()
        forgetWaitingShares()
        graph.servers.forget(maybeStored.address.origin)
        forgetActiveServer()
        checkServer(start.address)
      }
      start is ServerStart.CheckPreset -> checkServer(start.preset.address)
      else -> state.value = AppState.NeedsServer(prefill = "")
    }
  }

  /** The user entered (or pasted) a server address. */
  fun submitServer(input: String) = launchBusy {
    serverFromLink.value = false
    checkServer(input)
  }

  /** "Sign in there" on a link: the same check, and the confirmation warns who entered this server. */
  fun submitServerFromLink(host: String) = launchBusy {
    serverFromLink.value = true
    checkServer(host)
  }

  /**
   * The user confirmed the server: it becomes the account's. True when the browser sign-in
   * should follow; false when a session for it exists already (or nothing was to confirm).
   */
  suspend fun confirmServer(): Boolean {
    val current = state.value as? AppState.ConfirmServer ?: return false
    graph.servers.save(current.checked.server)
    // As stored: with the login hint of an earlier sign-in to this server kept (issue #448).
    val server = graph.servers.active() ?: current.checked.server
    withContext(Dispatchers.IO) { graph.auth.activate(server) }
    showServer(server)
    if (!graph.auth.isSignedIn.value) {
      // The page came with the answer just checked; the sign-in screen needn't ask again.
      showSignInPage(server.address.origin, current.checked.signInPage, isFresh = true)
      state.value = AppState.SignedOut()
      // Only the identity provider: straight to the browser, as before local accounts existed.
      return isSsoOnly(current.checked.signInPage.methods)
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
   * The browser intent for a sign-in the user starts; null (with the reason on the sign-in screen)
   * when the identity provider can't be reached.
   */
  suspend fun loginIntent(): Intent? {
    hasRetriedAuthorization = false
    return authorizationIntent()
  }

  /** A new authorization request: AppAuth gives each a fresh state and PKCE verifier. */
  private suspend fun authorizationIntent(): Intent? =
    try {
      graph.auth.loginIntent(page.value?.maybeLoginPageAlwaysShown)
    } catch (failure: Exception) {
      Log.w(TAG, signInFailureLog(failure, null), failure)
      val shown = signInFailure(failure, null, hasRetried = true) as? SignInFailure.Shown
      state.value = AppState.SignedOut(shown?.text ?: R.string.sign_in_failed)
      null
    }

  /**
   * Signs in to a local account of the active server. Every failure keeps the sign-in screen with
   * one reason; a wrong e-mail, password or code all read the same.
   */
  fun signInLocal(email: String, password: String, maybeCode: String?) = launchBusy {
    if (state.value !is AppState.SignedOut) return@launchBusy
    state.value = try {
      when (val outcome = graph.auth.signInLocal(email.trim(), password, maybeCode)) {
        is LocalLoginOutcome.SignedIn -> loadAccountState()
        else -> AppState.SignedOut(localSignInErrorText(outcome) ?: R.string.sign_in_failed)
      }
    } catch (failure: IOException) {
      Log.w(TAG, "local sign-in failed", failure)
      AppState.SignedOut(transportErrorText(failure, graph.network.isOnline.value))
    } catch (failure: IllegalStateException) {
      // The server changed while the request ran.
      Log.w(TAG, "local sign-in failed", failure)
      AppState.SignedOut(R.string.sign_in_failed)
    }
  }

  /**
   * The browser's answer. Failures of the sign-in itself stay on the sign-in screen with their
   * reason, or start the one automatic retry (issue #1063); loading the account afterwards goes
   * through the normal error texts.
   */
  fun completeLogin(result: Intent?) = launchBusy {
    try {
      graph.auth.completeLogin(result)
    } catch (failure: Exception) {
      val maybeRefusal = providerRefusal(failure)
      Log.w(TAG, signInFailureLog(failure, maybeRefusal), failure.takeIf { maybeRefusal == null })
      when (val outcome = signInFailure(failure, maybeRefusal, hasRetriedAuthorization)) {
        SignInFailure.Retry -> retryAuthorization()
        SignInFailure.Cancelled -> state.value = AppState.SignedOut()
        is SignInFailure.Shown -> state.value = AppState.SignedOut(outcome.text, outcome.maybeCode)
      }
      return@launchBusy
    }
    state.value = loadAccountState()
  }

  /** Goes to the identity provider once more; the activity opens the browser. */
  private suspend fun retryAuthorization() {
    hasRetriedAuthorization = true
    Log.i(TAG, "retrying the sign-in once with a new authorization request")
    authorizationIntent()?.let { loginRetries.send(it) }
  }

  fun enroll(passphrase: String) = launchBusy {
    val current = state.value as? AppState.NeedsEnrollment ?: return@launchBusy
    state.value = try {
      ready(graph.core.core().enroll(deviceName(), passphrase))
    } catch (failure: TeamCraftException) {
      Log.w(TAG, "enrollment failed", failure)
      current.copy(maybeError = errorText(failure))
    }
  }

  /** The running link of this (new) phone; a scan or a fresh start replaces it. */
  private var linkJob: Job? = null

  /**
   * New phone: waits in the relay so the other devices can be asked (no code to show: nobody could
   * scan it, issue #998). A wait that ran out starts again by itself, saying why.
   */
  fun linkThisDevice(@StringRes maybeNotice: Int? = null) {
    val attempt = ((state.value as? AppState.LinkingThisDevice)?.attempt ?: 0) + 1
    startLink(AppState.LinkingThisDevice(attempt = attempt, maybeNotice = maybeNotice)) { core ->
      core.startDeviceLink(deviceName(), LinkPlatform.ANDROID)
      core.waitForLinkPeer()
    }
  }

  /** New phone: scanned the code a signed-in computer shows for it ("Link a phone"). */
  fun linkScannedCode(code: String) {
    val current = state.value as? AppState.LinkingThisDevice ?: return
    startLink(current.copy(maybeNotice = null)) { core -> core.joinScannedDeviceLink(code.trim()) }
  }

  private fun startLink(waiting: AppState.LinkingThisDevice, peer: suspend (TeamCraft) -> String) {
    linkJob?.cancel()
    linkJob = launchBusy {
      // The relay connects before any request asks for a token.
      NetworkTrust.awaitTrustedCertificates()
      val core = graph.core.core()
      state.value = waiting
      try {
        val shortCode = peer(core)
        state.value = waiting.copy(maybeShortCode = shortCode, maybeNotice = null)
        state.value = ready(core.finishDeviceLink(deviceName()))
      } catch (failure: TeamCraftException.LinkCodeExpired) {
        Log.i(TAG, "the device link code expired", failure)
        if (waiting.maybeShortCode == null) linkThisDevice(R.string.link_expired_restarted) else linkFailed(failure)
      } catch (failure: TeamCraftException.LinkCodeForSignedInDevice) {
        // The wait this replaced lost its listener: start a fresh one, saying what was wrong.
        linkThisDevice(R.string.link_code_for_signed_in_device)
      } catch (failure: TeamCraftException.LinkCodeInvalid) {
        linkThisDevice(R.string.link_code_invalid)
      } catch (failure: TeamCraftException) {
        linkFailed(failure)
      }
    }
  }

  private fun linkFailed(failure: TeamCraftException) {
    Log.w(TAG, "linking this device failed", failure)
    state.value = AppState.NeedsEnrollment(hasExistingIdentity = true, maybeError = errorText(failure))
  }

  /** Asks the organization's admins to recover the account; shows the code to read to one. */
  fun recoverThroughOrganization() = launchBusy {
    state.value = try {
      AppState.RecoveringThroughOrganization(graph.core.core().startOrgRecovery().verificationCode)
    } catch (failure: TeamCraftException) {
      Log.w(TAG, "the recovery request could not start", failure)
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
      Log.w(TAG, "recovery through the organization failed", failure)
      current.copy(maybeError = errorText(failure))
    }
  }

  fun cancelLinking() {
    linkJob?.cancel()
    linkJob = null
    state.value = AppState.NeedsEnrollment(hasExistingIdentity = true)
  }

  /**
   * "Set up this device again" (issue #414): the core sets the unreadable database aside (renamed,
   * never deleted) and opens a new one; enrollment follows.
   */
  fun resetStore() = launchBusy {
    val current = state.value as? AppState.StoreUnreadable ?: return@launchBusy
    state.value = try {
      val core = graph.core.core()
      // Opens the new store while it waits.
      withContext(Dispatchers.IO) { core.resetStore() }
      loadAccountState()
    } catch (failure: TeamCraftException) {
      Log.w(TAG, "setting the local data aside failed", failure)
      current.copy(maybeError = errorText(failure))
    }
  }

  /** "Try again" on the unreadable-data screen: opens the core afresh (after an update, say). */
  fun retryStore() = launchBusy {
    if (state.value !is AppState.StoreUnreadable) return@launchBusy
    graph.core.close()
    state.value = loadAccountState()
  }

  /**
   * Signs out and removes what the account left in the clear on this device: notifications,
   * decrypted downloads and staged uploads, and the push registration. The sealed store stays.
   */
  fun signOut() = launchBusy {
    endSession()
    forgetWaitingShares()
    state.value = AppState.SignedOut()
  }

  /** The conversations as of the newest change, shared with every other screen that asks ([CoreRepository.conversations]). */
  suspend fun conversations(): List<ConversationItem> = runCatching { graph.core.conversations() }.getOrNull() ?: emptyList()

  /** Where a share can go (notes first, then by last activity), or null when it can't be read. */
  suspend fun shareTargets(): List<ConversationItem>? = read { shareTargets() }

  /** The user asked to read the chat list again: it shows as loading until the read ends. */
  fun retryChatList() = chatListState.update { it.copy(maybeReadFailure = null) }

  private suspend fun refreshTyping() {
    val places = readChat { query(ChatQueries.typingNow()).typing }.orEmpty().filter { it.users.isNotEmpty() }
    typingPlacesState.value = places
    val inConversations = places.filter { it.maybeThread == null }
    if (inConversations.isEmpty()) {
      typingNamesState.value = emptyMap()
      return
    }
    // The kept snapshot has the names unless someone new is typing: then it is read afresh.
    val ids = inConversations.flatMap { it.users }
    var snapshot = chat.maybeKeptSnapshot()
    if (snapshot == null || ids.any { snapshot?.user(it) == null }) snapshot = readChat { snapshot() } ?: snapshot
    typingNamesState.value = typingNamesByConversation(places) { snapshot?.name(it).orEmpty() }
  }

  /**
   * Reads the conversations, shows them, then reads their previews. One refresh at a time. A read
   * that fails (the store still opening) keeps what is shown: an empty list would say "No direct
   * messages yet" (#1371); the next change reads again.
   */
  suspend fun refreshChatList(myId: String) = chatListLock.withLock {
    val conversations = runCatching { graph.core.conversations() }.getOrNull()
    if (conversations == null) {
      // A start whose chat list can't be read failed (#1375): its measure says so.
      Perf.chatListFailed()
      // Cached rows stay; with none, the list says it could not be read instead of loading for ever.
      chatListState.update { if (it.maybeConversations == null) it.copy(maybeReadFailure = LoadFailure.OTHER) else it }
      return@withLock
    }
    chatListState.update { it.copy(maybeConversations = conversations, maybeReadFailure = null, runningCalls = runningCalls()) }
    // The chat list shows the core's rows: a signed-in start is done (#1521).
    Perf.chatListShown()
    val previews = chatPreviews(conversations, myId)
    chatListState.update { it.copy(previews = previews) }
    isStartCacheStale = false
    rememberStartCache()
    readAhead()
  }

  /**
   * Media drafts and their camera shots of conversations the core purged or no longer lists
   * (#1161): deleted, then reported back. The core hands each out once and remembers it across a
   * restart until it is reported dropped.
   */
  private suspend fun dropDraftsOfDroppedConversations() {
    val core = runCatching { graph.core.core() }.getOrNull() ?: return
    val dropped = runCatching { core.droppedConversations() }.getOrNull().orEmpty()
    for (conversation in dropped) {
      val isDropped = MediaDraftStore.dropConversation(getApplication(), conversation)
      runCatching {
        if (isDropped) core.conversationFilesDropped(conversation) else core.conversationFilesKept(conversation)
      }
    }
  }

  /** What [conversationId] showed last or was read ahead with, to show before its first read. */
  fun feedSeed(conversationId: String): FeedSeed? = feedSeeds[conversationId]

  /** Keeps what an open conversation shows, so opening it again shows it in the first frame. */
  fun keepFeedSeed(conversationId: String, seed: FeedSeed) = feedSeeds.keep(conversationId, seed)

  /** Drops what [conversationId] showed last: it is older than what is about to open it. */
  fun forgetFeedSeed(conversationId: String) = feedSeeds.forget(conversationId)

  /**
   * Reads the newest messages of the conversations at the top of the list ahead, so they open with
   * their messages in the first frame. Only those whose newest message changed, one at a time on the
   * read thread. A running pass isn't restarted (while syncing the list changes faster than a pass
   * takes, and it would never finish): it looks at the list again when it is through.
   */
  private fun readAhead() {
    if (maybeReadAhead?.isActive == true) return
    maybeReadAhead = viewModelScope.launch {
      // Twice at most: once, and once more for what changed meanwhile; later changes start another.
      repeat(READ_AHEAD_PASSES) {
        val ids = feedSeeds.toReadAhead(chatListState.value)
        if (ids.isEmpty() || !readAhead(ids)) return@launch
      }
    }
  }

  /** `false` when a read failed (the store closed, say): the next list change tries again. */
  private suspend fun readAhead(ids: List<String>): Boolean {
    for (id in ids) {
      val tail = readChat { tailAhead(id, FEED_SEED_MESSAGES) } ?: return false
      val extras = readChat { extras(id, tail.messages.map { it.id }) } ?: emptyMap()
      val maybeUnread = readChat { query(ChatQueries.firstUnread(id)).maybeMessage }
      val firstUnread = maybeUnread?.let { FirstUnread.At(it) } ?: FirstUnread.None
      feedSeeds.keep(id, FeedSeed(tail.messages, tail.hasOlder, extras, firstUnread))
    }
    return true
  }

  /**
   * Shows the account and chat list of the last run at once, before the store is open; the real
   * state replaces them when it is read. Read on the main thread on purpose: a few milliseconds
   * that make the first frame the list instead of a loading screen. Returns the account it showed.
   */
  private fun showStartCache(): String? {
    val cached = runCatching { startCache.read() }
      .onFailure { Log.w(TAG, "start cache not read: ${it.javaClass.simpleName}") }
      .getOrNull() ?: return null
    chatListState.value = cached.listState()
    state.value = cached.ready()
    return cached.account
  }

  /** One list pass when the app leaves after changes the list didn't see; it rewrites the start cache. */
  private fun refreshStartCacheIfStale() {
    val ready = state.value as? AppState.Ready ?: return
    if (!isStartCacheStale) return
    viewModelScope.launch { refreshChatList(ready.userId) }
  }

  /** Drops what the session showed from memory: the list, read-ahead chats, the kept snapshot. */
  private fun forgetSession() {
    maybeStartCacheWrite?.cancel()
    maybeReadAhead?.cancel()
    chatListState.value = ChatListState()
    feedSeeds.clear()
    chat.forget()
  }

  /** Keeps the list for the next start, once changes have settled for a moment. */
  private fun rememberStartCache() {
    val ready = state.value as? AppState.Ready ?: return
    val list = chatListState.value
    maybeStartCacheWrite?.cancel()
    maybeStartCacheWrite = viewModelScope.launch(Dispatchers.IO) {
      delay(START_CACHE_SETTLE_MILLIS)
      val account = accountKey() ?: return@launch
      val cache = StartCache.of(account, ready, list) ?: return@launch
      // Checked again under the store's lock: a sign-out may have deleted it since.
      runCatching { startCache.write(cache) { isActive && accountKey() == account } }
        .onFailure { Log.w(TAG, "start cache not written: ${it.javaClass.simpleName}") }
    }
  }

  /** The signed-in account as the store is keyed (`CoreRepository`), or `null` when signed out. */
  private fun accountKey(): String? {
    val origin = graph.auth.server?.address?.origin ?: return null
    val subject = graph.auth.account ?: return null
    return "$origin $subject"
  }

  /**
   * The line under each conversation of the chat list, in one read of the local store for the whole
   * list (the newest message people wrote; joins and renames only when nothing was written nearby).
   */
  suspend fun chatPreviews(conversations: List<ConversationItem>, myId: String): Map<String, ChatListEntry> {
    val isOneToOne = conversations.associate { it.id to (it.kind == ConversationKind.DIRECT) }
    val items = read { chatPreviews(conversations.map { it.id }) } ?: return emptyMap()
    return items.associate { item ->
      val message = namedAuthor(item.message)
      item.conversationId to ChatListEntry(chatPreview(message, myId, isOneToOne[item.conversationId] == true), message.id)
    }
  }

  /**
   * What the newest notification showed of a conversation, while the store hasn't caught it up yet
   * (issue #1065): its name and the notified message, so opening it never shows an empty screen.
   */
  suspend fun notifiedConversation(conversationId: String): NotifiedConversation? =
    read { notifiedConversation(conversationId) }

  /** How far the first sync of a newly set up device is; `null` when none runs. */
  suspend fun firstSync(): FirstSyncItem? = read { firstSync() }

  /** How far conversations waiting for message keys have caught up (issue #888); `null` while none waits. */
  suspend fun keyCatchUp(): KeyCatchUpItem? = read { keyCatchUp() }

  suspend fun messages(conversationId: String): List<MessageItem> =
    read { messages(conversationId, MESSAGE_PAGE.toUInt()) }?.map(::namedAuthor) ?: emptyList()

  suspend fun thread(conversationId: String, threadId: String): List<MessageItem> =
    read { thread(conversationId, threadId) }?.map(::namedAuthor) ?: emptyList()

  /** The hits for [query]; `null` when the search could not run, which is not "no hits" (#297). */
  suspend fun search(query: String): List<SearchHitItem>? =
    if (query.isBlank()) {
      emptyList()
    } else {
      read { search(query) }?.map { it.copy(message = namedAuthor(it.message)) }
    }

  /** The app's own "Deleted user" in the user's language (issue #751). */
  private fun deletedUserName(): String = getApplication<Application>().getString(R.string.user_deleted_name)

  /** [message] with [deletedUserName] instead of the server's placeholder when its author was deleted. */
  private fun namedAuthor(message: MessageItem): MessageItem =
    if (message.isAuthorDeleted) message.copy(authorName = deletedUserName()) else message

  fun send(conversationId: String, maybeThreadId: String?, body: String) = viewModelScope.launch {
    runCatching { graph.core.core().sendMessage(conversationId, maybeThreadId, body) }
  }

  fun markRead(conversationId: String) = viewModelScope.launch {
    runCatching { graph.core.core().markRead(conversationId) }
    graph.notifier.dismiss(conversationId)
  }

  /** Sets the status; with [maybeUntil] it is automatic again from then ("Reset after", #1578). */
  fun setPresence(presence: Presence, maybeUntil: java.time.Instant? = null) = viewModelScope.launch {
    runCatching {
      val core = graph.core.core()
      if (maybeUntil == null) core.setPresence(presence) else core.setPresenceUntil(presence, maybeUntil.toEpochMilli())
    }
  }

  /** `{origin}/c/{id}` of a conversation on the active server, if it has a web address (#1105). */
  suspend fun conversationLink(conversationId: String): String? {
    val origin = graph.servers.active()?.address?.origin ?: return null
    return chat.conversationLink(origin, conversationId)
  }

  /** Whether the menu offers "Leave" for this conversation: the core's rule. */
  suspend fun takeSuspectDeletions(): UInt = readChat { takeSuspectDeletions() } ?: 0u

  suspend fun leaveChoice(conversationId: String): LeaveChoice =
    readChat { leaveChoice(conversationId) } ?: LeaveChoice.HIDDEN

  suspend fun spaceMenu(spaceId: String): SpaceMenuItem? = readChat { spaceMenu(spaceId) }

  /** A channel's members section (#1262); `null` when the core can't answer. */
  suspend fun channelMembers(conversationId: String): ChannelMembersInfo? = readChat { channelMembers(conversationId) }

  /** The members section opened: the members and signed roles are read afresh, as on desktop and web. */
  fun refreshChannelMembers(conversationId: String) = viewModelScope.launch {
    reportFailure { chat.refreshChannelMembers(conversationId) }
  }

  /** Confirms the roles a workspace admin looked at; `null` after a failure, shown as [lastFailure]. */
  suspend fun signReviewedChannelRoles(conversationId: String, review: ChannelRolesReviewItem): ReviewedRolesOutcome? =
    try {
      chat.signReviewedChannelRoles(conversationId, review)
    } catch (failure: TeamCraftException) {
      failures.value = errorText(failure)
      null
    }

  fun markSpaceRead(spaceId: String) = viewModelScope.launch {
    reportFailure { graph.core.core().markSpaceRead(spaceId) }
  }

  fun leaveSpace(spaceId: String) = viewModelScope.launch {
    reportFailure { graph.core.core().leaveSpace(spaceId) }
  }

  /** Mutes the space, or unmutes it with `null`; a failure shows as [lastFailure]. */
  fun muteSpace(spaceId: String, maybeUntil: Long?) = viewModelScope.launch {
    reportFailure { chat.setSpaceMutedUntil(spaceId, maybeUntil) }
  }

  suspend fun spaceMutedUntil(spaceId: String): Long? =
    readChat { spaceMutedUntil(spaceId, System.currentTimeMillis()) }

  private suspend fun reportFailure(block: suspend () -> Unit) {
    try {
      block()
    } catch (failure: TeamCraftException) {
      failures.value = errorText(failure)
    } catch (failure: CancellationException) {
      throw failure
    } catch (failure: IllegalStateException) {
      // Signed out meanwhile: no account to act for.
    }
  }

  /** Runs a `ChatAction` built with [ChatActions]; failures show as [lastFailure]. */
  fun act(action: String, onRefused: () -> Unit = {}) = viewModelScope.launch {
    try {
      chat.act(action)
    } catch (failure: TeamCraftException) {
      failures.value = errorText(failure)
      onRefused()
    } catch (failure: CancellationException) {
      throw failure
    } catch (failure: IllegalStateException) {
      // Signed out meanwhile (a draft saved after the delay, say): no account to act for.
    }
  }

  /**
   * Sends a composer's message as [attempt] (#1579). In the view model's scope: leaving the
   * conversation doesn't stop it. [onSent] runs once the core has the message; a failure stays on
   * the composer ([ComposerSend]) with its reason, for a retry with the same message ID.
   */
  fun sendFromComposer(key: String, attempt: SendAttempt, action: String, onSent: () -> Unit) = viewModelScope.launch {
    @StringRes val maybeFailure = try {
      tryAct(action)
    } catch (failure: CancellationException) {
      throw failure
    } catch (failure: RuntimeException) {
      // Anything else the bridge throws must still reach the user, not end the send in silence.
      Log.w(TAG, "send failed: ${failure.javaClass.simpleName}")
      R.string.error_generic
    }
    if (maybeFailure == null) {
      composerSend.succeeded(key, attempt)
      onSent()
    } else {
      composerSend.failed(key, attempt, maybeFailure)
    }
  }

  /**
   * "Cancel all" on a message's files going up (#1579). The core finds a file by its place among
   * the message's pending files, and a cancel takes it out: the last goes first, one after another,
   * so every place still names its file.
   */
  fun cancelUploads(files: List<FileInfo>) = viewModelScope.launch {
    val last = files.sortedByDescending { it.fileId.removePrefix("pending-").toIntOrNull() ?: -1 }
    for (file in last) {
      tryAct(ChatActions.cancelUpload(file.conversation, file.message, file.fileId))?.let { failure ->
        failures.value = failure
        return@launch
      }
    }
  }

  /**
   * Runs [action] for a form and returns why it failed (null on success), so the form shows the
   * refusal at its field and stays open with the input (forms.md, "Submitting").
   */
  @StringRes
  suspend fun tryAct(action: String): Int? =
    try {
      chat.act(action)
      null
    } catch (failure: TeamCraftException) {
      errorText(failure)
    } catch (failure: IllegalStateException) {
      // Signed out meanwhile: the sign-in screen replaces the form.
      R.string.error_signed_out
    }

  /**
   * Runs a create call of a form (#1534): the new conversation's or space's ID, a refusal of the
   * name for its field, or why it failed otherwise. The form stays open unless it was created.
   */
  suspend fun tryCreate(create: suspend ChatClient.() -> CreateOutcome): CreateResult =
    try {
      when (val outcome = withContext(Dispatchers.Default) { chat.create() }) {
        is CreateOutcome.Created -> CreateResult.Created(outcome.id)
        is CreateOutcome.NameRefused -> CreateResult.NameRefused
        is CreateOutcome.NotAllowed -> CreateResult.Failed(R.string.create_not_allowed)
      }
    } catch (failure: CancellationException) {
      throw failure
    } catch (failure: TeamCraftException) {
      CreateResult.Failed(errorText(failure))
    } catch (failure: IllegalStateException) {
      CreateResult.Failed(R.string.error_signed_out)
    }

  /** Adds people to a channel (#1534); answers why it failed, or null once everybody is in. */
  @StringRes
  suspend fun tryAddPeople(conversation: String, users: List<String>): Int? =
    try {
      when (withContext(Dispatchers.Default) { chat.addPeopleToChannel(conversation, users) }) {
        AddPeopleOutcome.ADDED -> null
        AddPeopleOutcome.NOT_IN_SPACE -> R.string.members_add_not_in_space
        AddPeopleOutcome.NOT_ALLOWED -> R.string.create_not_allowed
      }
    } catch (failure: CancellationException) {
      throw failure
    } catch (failure: TeamCraftException) {
      errorText(failure)
    } catch (failure: IllegalStateException) {
      R.string.error_signed_out
    }

  /** Whether "New space" shows; false while the server can't tell (offline). */
  suspend fun canCreateSpaces(): Boolean = readChat { canCreateSpaces() } ?: false

  /**
   * Saves the member's own name (`null`: the sign-in name). Answers the string to show at the field
   * when it failed. A refusal of the server (too long, or the policy changed meanwhile) is worded
   * for the name; no connection and the like are worded as everywhere.
   */
  @StringRes
  suspend fun trySetDisplayName(profile: OwnProfile, maybeName: String?): Int? =
    try {
      chat.setDisplayName(profile, maybeName)
      null
    } catch (failure: TeamCraftException.Core) {
      R.string.profile_name_rejected
    } catch (failure: TeamCraftException) {
      errorText(failure)
    } catch (failure: IllegalStateException) {
      R.string.error_signed_out
    }

  /** Whose public holidays the member's repeating meetings skip; `null` when the server can't tell now. */
  suspend fun loadHolidayRegion(): HolidayRegion? = readChat { holidayRegion() }

  /**
   * Picks the holiday region (`null`: the workspace's): what the server stores now, or the text
   * that says why it wasn't saved. The meeting list reads the meetings afresh whenever it opens.
   */
  suspend fun trySetHolidayRegion(maybeRegion: String?): Pair<HolidayRegion?, Int?> =
    try {
      withContext(Dispatchers.Default) { chat.setHolidayRegion(maybeRegion) } to null
    } catch (failure: CancellationException) {
      throw failure
    } catch (failure: TeamCraftException.Core) {
      null to R.string.holiday_save_failed
    } catch (failure: TeamCraftException) {
      null to errorText(failure)
    } catch (failure: kotlinx.serialization.SerializationException) {
      null to R.string.holiday_save_failed
    } catch (failure: IllegalStateException) {
      null to R.string.error_signed_out
    }

  /**
   * Forwards one file of a message. The core sends nothing for a file that went meanwhile (never
   * the rest of the album instead), so that is told here.
   */
  fun forwardFile(file: FileInfo, to: String) = viewModelScope.launch {
    try {
      val files = chat.extras(file.conversation, listOf(file.message))[file.message]?.files.orEmpty()
      if (files.any { it.fileId == file.fileId }) {
        chat.act(ChatActions.forward(file.conversation, file.message, to, "", file.fileId))
      } else {
        failures.value = R.string.file_gone
      }
    } catch (failure: TeamCraftException) {
      failures.value = errorText(failure)
    } catch (failure: IllegalStateException) {
      // Signed out meanwhile: no account to act for.
    }
  }

  /** Shows a message once, like a failed action. */
  fun showFailure(@StringRes message: Int) {
    failures.value = message
  }

  /** Where a message link points, if this device is in that conversation and has the message. */
  suspend fun resolvePermalink(link: String): PermalinkTarget? =
    readChat { query(ChatQueries.resolvePermalink(link)).maybeTarget }

  /** The failure to show, taken so that only one screen shows it; `null` when another took it. */
  fun takeFailure(): Int? = failures.getAndUpdate { null }

  /**
   * Asks the server for one page of a directory list (#1337): the page, or why it failed, so the
   * list can say so and offer another try.
   */
  suspend fun <T> fetchPage(block: suspend ChatClient.() -> T): PageAnswer<T> =
    try {
      PageAnswer.Loaded(withContext(Dispatchers.Default) { chat.block() })
    } catch (failure: CancellationException) {
      throw failure
    } catch (failure: TeamCraftException) {
      PageAnswer.Failed(if (failure is TeamCraftException.Network) LoadFailure.NETWORK else LoadFailure.OTHER)
    } catch (failure: IllegalStateException) {
      PageAnswer.Failed(LoadFailure.OTHER)
    }

  /**
   * Runs a directory action (invite, resend, revoke): its outcome, or the text of why it failed
   * when the server didn't answer it.
   */
  suspend fun <T> tryDirectoryAction(block: suspend ChatClient.() -> T): ActionAnswer<T> =
    try {
      ActionAnswer.Answered(withContext(Dispatchers.Default) { chat.block() })
    } catch (failure: CancellationException) {
      throw failure
    } catch (failure: TeamCraftException) {
      ActionAnswer.Failed(errorText(failure))
    } catch (failure: IllegalStateException) {
      ActionAnswer.Failed(R.string.error_signed_out)
    }

  /** Runs a read; `null` when the core can't answer (not enrolled, gone). */
  suspend fun <T> readChat(block: suspend ChatClient.() -> T): T? =
    try {
      // Off the main thread: decoding a snapshot or a page of messages takes milliseconds that a
      // frame doesn't have (#315). Screens call this from their composition's effects.
      withContext(Dispatchers.Default) { chat.block() }
    } catch (failure: TeamCraftException) {
      null
    } catch (failure: kotlinx.serialization.SerializationException) {
      null
    } catch (failure: CancellationException) {
      // A CancellationException is an IllegalStateException; the caller's cancellation goes on.
      throw failure
    } catch (failure: IllegalStateException) {
      // No server or no account any more (signed out while a screen still reads): nothing to show.
      null
    }

  fun pickedKey(conversation: String, maybeThread: String?) = "$conversation/${maybeThread.orEmpty()}"

  /**
   * Attaches the composer's picked files that aren't in its draft yet (#1220): the draft keeps them
   * after the app is killed, and they go up when the message is sent (#1585). Runs in the view
   * model's scope, so leaving the conversation doesn't stop it halfway; a file sent or removed
   * meanwhile is not attached.
   */
  fun attachPickedToDraft(conversation: String, maybeThread: String?) = viewModelScope.launch {
    val key = pickedKey(conversation, maybeThread)
    val fresh = pickedFiles[key].orEmpty().filter { it.maybeDraft == null && draftAttaching.add(it.path) }
    if (fresh.isEmpty()) return@launch
    try {
      val maybeStaging = withContext(Dispatchers.IO) { runCatching { graph.core.uploadStagingDir() }.getOrNull() }
      val staging = maybeStaging ?: return@launch
      val prepared = withContext(Dispatchers.IO) { fresh.map { it to DraftFiles.copyFor(it, staging) } }
      // Back on the main thread: the composer may have sent or removed some meanwhile. While its
      // send runs, the files go with the message: the draft doesn't take them as well (#1579).
      val isSending = composerSend.state(key) is ComposerSendState.Sending
      val merged = DraftFiles.merge(if (isSending) emptyList() else pickedFiles[key].orEmpty(), prepared)
      if (merged.attached.isNotEmpty()) pickedFiles[key] = merged.files
      if (merged.dropped.isNotEmpty()) {
        withContext(Dispatchers.IO) { merged.dropped.forEach { deleteStaged(it.path) } }
      }
      if (merged.attached.isNotEmpty()) act(ChatActions.attachDraftFiles(conversation, maybeThread, merged.attached))
    } finally {
      fresh.forEach { draftAttaching.remove(it.path) }
    }
  }

  /** A picked file taken out of the composer: out of the draft too, and its staged copies go. */
  fun removePicked(conversation: String, maybeThread: String?, file: PickedFile) {
    file.maybeDraft?.let { removeDraftFile(conversation, maybeThread, it.id) }
    viewModelScope.launch(Dispatchers.IO) { DraftFiles.pathsOfRemoved(file).forEach { deleteStaged(it) } }
  }

  /** Takes a file out of the draft on every device; it doesn't show again meanwhile. */
  fun removeDraftFile(conversation: String, maybeThread: String?, id: String) {
    forgetDraftFiles(listOf(id))
    act(ChatActions.removeDraftFile(conversation, maybeThread, id))
  }

  /**
   * The composer's files went with a message: the draft's files don't show again while the core
   * catches up, and the composer's own names of those that went from the draft's copy go.
   */
  fun pickedSent(sent: List<PickedFile>, draftIds: List<String>) {
    forgetDraftFiles(sent.mapNotNull { it.maybeDraft?.id } + draftIds)
    val left = DraftFiles.leftAfterSend(sent)
    if (left.isNotEmpty()) viewModelScope.launch(Dispatchers.IO) { left.forEach { deleteStaged(it) } }
  }

  private fun forgetDraftFiles(ids: List<String>) {
    goneDraftFiles.addAll(ids.filter { it !in goneDraftFiles })
    while (goneDraftFiles.size > MAX_GONE_DRAFT_FILES) goneDraftFiles.removeAt(0)
  }

  /** Deletes a staged copy; only files in the account's staging folder are the app's to delete. */
  private suspend fun deleteStaged(path: String) {
    val directory = runCatching { graph.core.uploadStagingDir().canonicalFile }.getOrNull() ?: return
    val staged = File(path).canonicalFile
    if (staged.parentFile == directory) staged.delete()
  }

  /**
   * Deletes the staged copy of a file taken out of the composer before sending. Only files in the
   * account's staging folder; anything else is not the app's to delete.
   */
  fun discardFile(file: PickedFile) = viewModelScope.launch(Dispatchers.IO) { deleteStaged(file.path) }

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

  /**
   * Copies a share into the inbox right away (a URI grant doesn't outlive the task, the copy does).
   * [isRedelivery]: the intent of a recreated activity, whose copy may be under way or done.
   */
  fun receiveShare(content: SharedContent, isRedelivery: Boolean = false) {
    if (shareState.value == null) shareState.value = ShareState.Preparing
    shares.receive(content, isRedelivery) { shareSources(content) }
  }

  /** The waiting share goes without a conversation: the user said so. */
  fun discardShare() {
    val maybeId = (shareState.value as? ShareState.Ready)?.share?.id
    viewModelScope.launch {
      if (maybeId == null) shares.stopCopies()
      maybeId?.let { id -> withContext(Dispatchers.IO) { shares.inbox.delete(id) } }
      refreshShare()
    }
  }

  /** The user picked a conversation: its composer takes over ([finishShare]). */
  fun handOverShare(share: StagedShare) {
    shares.handOver(share)
    viewModelScope.launch { refreshShare() }
  }

  /** Moves a shared file into the core's staging folder; `null` when that fails. */
  suspend fun importSharedFile(share: StagedShare, file: StagedShareFile): PickedFile? = withContext(Dispatchers.IO) {
    val source = shares.inbox.fileOf(share, file)
    val name = cleanFileName(file.name).ifBlank { "file" }
    val directory = runCatching { graph.core.uploadStagingDir() }.getOrNull() ?: return@withContext null
    val target = File(directory, "${System.nanoTime()}-${name.replace('/', '_')}")
    if (source.renameTo(target)) {
      withVideoPoster(PickedFile(fileName = name, mediaType = file.mediaType, path = target.absolutePath))
    } else {
      stage(name, file.mediaType) { source.inputStream() }
    }
  }

  /** The composer has the share: it leaves the inbox, except [failed] files, which wait for a retry. */
  suspend fun finishShare(share: StagedShare, failed: List<StagedShareFile>) {
    withContext(Dispatchers.IO) {
      if (failed.isEmpty()) shares.inbox.delete(share.id) else shares.inbox.keepOnly(share, failed)
    }
    shares.finished(share)
    refreshShare()
  }

  /** What waits to be shared doesn't outlive the session on this device. */
  private suspend fun forgetWaitingShares() {
    sendAttempts.wipe()
    shares.stopCopies()
    withContext(Dispatchers.IO) { shares.inbox.wipe() }
    shares.forgetHandedOver()
    PendingShare.current.value = null
    shareState.value = null
  }

  private suspend fun refreshShare() {
    val maybeNext = withContext(Dispatchers.IO) { shares.next() }
    shareState.value = maybeNext?.let { ShareState.Ready(it) } ?: ShareState.Preparing.takeIf { shares.isCopying }
  }

  /** The share's files, each checked and named like a picked file; others count as left out. */
  private fun shareSources(content: SharedContent): List<ShareSource> {
    val app = getApplication<Application>()
    val resolver = app.contentResolver
    return content.uris.map { uri ->
      // A share from another app may not point into the app's private files.
      val isAllowed = isForeignContent(uri.scheme, uri.authority, app.packageName) && !isPrivateFile(uri)
      val maybeName = runCatching {
        resolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { cursor ->
          if (cursor.moveToFirst()) cursor.getString(0) else null
        }
      }.getOrNull()
      val name = maybeName?.let(::cleanFileName)?.takeIf { it.isNotBlank() } ?: "file"
      val type = runCatching { resolver.getType(uri) }.getOrNull() ?: "application/octet-stream"
      ShareSource(name, type) { if (isAllowed) resolver.openInputStream(uri) else null }
    }
  }

  /** An edited picture ([de.zeitgleich.teamcraft.ui.screens.PictureEditor]) staged like a picked file. */
  suspend fun stageEditedPicture(name: String, mediaType: String, bytes: ByteArray): PickedFile? = withContext(Dispatchers.IO) {
    stage(name, mediaType) { bytes.inputStream() }
  }

  /**
   * A file the app made itself (an exported video, a camera recording) staged like a picked file:
   * moved when it can be, else copied and deleted.
   */
  suspend fun stageMovedFile(name: String, mediaType: String, file: File): PickedFile? = withContext(Dispatchers.IO) {
    val directory = runCatching { graph.core.uploadStagingDir() }.getOrNull() ?: return@withContext null
    val target = File(directory, "${System.nanoTime()}-${cleanFileName(name).replace('/', '_')}")
    if (file.length() <= MAX_IMPORT_BYTES && file.renameTo(target)) {
      return@withContext withVideoPoster(PickedFile(fileName = name, mediaType = mediaType, path = target.absolutePath))
    }
    val maybeStaged = stage(name, mediaType) { file.inputStream() }
    file.delete()
    maybeStaged
  }

  /**
   * A sealed video of the media flow staged like a picked file: decrypted straight into the staging
   * folder, the one plain copy it gets ([uri] stays as it is).
   */
  suspend fun stageSealedFile(name: String, mediaType: String, uri: Uri): PickedFile? = withContext(Dispatchers.IO) {
    stage(cleanFileName(name), mediaType) { SealedCaptures.inputStream(getApplication<Application>(), uri) }
  }

  /** Copies into the core's staging folder, at most [MAX_IMPORT_BYTES]. */
  private suspend fun stage(name: String, type: String, open: () -> InputStream?): PickedFile? {
    // The core deletes the copy once it is uploaded (or failed for good).
    val directory = runCatching { graph.core.uploadStagingDir() }.getOrNull() ?: return null
    val target = File(directory, "${System.nanoTime()}-${name.replace('/', '_')}")
    val isCopied = runCatching {
      open()?.use { input -> target.outputStream().use { copyAtMost(input, it, MAX_IMPORT_BYTES) } } ?: false
    }.getOrDefault(false)
    if (!isCopied) {
      target.delete()
      return null
    }
    return withVideoPoster(PickedFile(fileName = name, mediaType = type, path = target.absolutePath))
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

  /**
   * Where the media flow's camera writes (`media-flow-captures-…` folders): the core's staging
   * folder, wiped at sign-out with the other plaintext copies. `null` while no account is open.
   */
  suspend fun mediaCapturesRoot(): File? = runCatching { graph.core.uploadStagingDir() }.getOrNull()

  /** Conversations with a call running now and how many people are in each (issue #1352). */
  suspend fun runningCalls(): Map<String, Int> =
    read { runningCalls() }?.associate { it.conversationId to it.participantUserIds.size } ?: emptyMap()

  /** Brings the call screen of this device's call to the front. */
  fun showCall() = graph.calls.bringToFront()

  /** Joins (or starts) a conversation's call, e.g. a meeting's. */
  fun startCall(conversationId: String, displayName: String, options: CallOptions = CallOptions()) =
    graph.calls.startCall(conversationId, displayName, options)

  /**
   * Whether the call running in `conversationId` has the notetaker in it, for the pre-join notice
   * (e2ee.md §8.2): who it is comes from the verified policy, who is in the call from the roster.
   */
  suspend fun isNotetakerInRunningCall(conversationId: String): Boolean =
    read { notetakerInCall(conversationId, callParticipants(conversationId))?.isPresent } ?: false

  /**
   * Where the call of `conversationId` moved to, for the pre-join dialog (issue #1321): the room
   * the user is in whose call runs now, or `null` when nothing moved or it is over.
   */
  suspend fun movedCallHint(conversationId: String): MovedCallHint? =
    read { movedCallHint(conversationId) }

  /** Ask before `@channel` in conversations with more members than this (0 = never). */
  fun confirmChannelOver(): Int = graph.mentionPrefs.confirmChannelOver

  val mentionPrefs get() = graph.mentionPrefs

  val notificationContent get() = graph.notificationContent

  /** Sets how much notifications show; those already in the shade follow at once. */
  fun setNotificationContent(content: NotificationContent) {
    graph.notificationContent.update(content)
    graph.notifier.repostAll()
    viewModelScope.launch { read { setNotificationContent(content) } }
  }

  /** The workspace policy's limit for notifications; `null` when the core can't answer. */
  suspend fun notificationContentLimit(): NotificationContent? = read { notificationContent().workspace }

  val segmentPrefetch get() = graph.segmentPrefetch

  /** Crash reports: the privacy switch and the first-start notice. */
  val crashReports get() = graph.crashReports

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

  private val linkRequestList = MutableStateFlow<List<LinkRequestItem>>(emptyList())

  /** New devices of this account that wait for a decision; the prompts on every signed-in screen. */
  val linkRequests: StateFlow<List<LinkRequestItem>> = linkRequestList

  private val review = MutableStateFlow<LinkReview?>(null)

  /** The approval the user took up from a prompt; absent while none is open. */
  val linkReview: StateFlow<LinkReview?> = review

  private val promptFailure = MutableStateFlow<Int?>(null)

  /** Why denying or reviewing a request failed, shown on the prompt. */
  val linkPromptFailure: StateFlow<Int?> = promptFailure

  /** Reads the waiting requests; cheap, called on every store change. */
  suspend fun refreshLinkRequests() {
    read { linkRequests() }?.let { linkRequestList.value = it }
  }

  /** Drops a prompt whose request ran out; a later read confirms it. */
  fun expireLinkRequest(id: String) {
    linkRequestList.value = linkRequestList.value.filter { it.id != id }
  }

  fun rejectLinkRequest(id: String) = viewModelScope.launch {
    promptFailure.value = null
    try {
      graph.core.core().rejectLinkRequest(id)
      expireLinkRequest(id)
    } catch (failure: TeamCraftException) {
      promptFailure.value = errorText(failure)
    }
  }

  /** Takes up a request from its prompt; the user then types the code the new device shows. */
  fun reviewLinkRequest(request: LinkRequestItem) = viewModelScope.launch {
    promptFailure.value = null
    try {
      graph.core.core().reviewLinkRequest(request.id)
      expireLinkRequest(request.id)
      review.value = LinkReview(request.deviceName)
    } catch (failure: TeamCraftException) {
      promptFailure.value = errorText(failure)
    }
  }

  /** Sends the typed code of a reviewed request; a wrong code stays on the step for another try. */
  fun confirmLinkReview(typedCode: String) = viewModelScope.launch {
    val current = review.value?.copy(isConfirming = true) ?: return@launch
    review.value = current
    val confirmation = confirmLink(typedCode)
    // Closing the review while the new device's receipt is awaited cancels the wait (issue #415):
    // a review the user closed (or replaced) meanwhile stays closed, without an error.
    if (review.value !== current) return@launch
    review.value = when (confirmation) {
      LinkConfirmation.CONFIRMED -> current.copy(maybeError = null, isDone = true)
      LinkConfirmation.WRONG_CODE -> current.copy(maybeError = R.string.approve_code_wrong)
      LinkConfirmation.ATTEMPT_ENDED -> current.copy(maybeError = R.string.approve_attempt_ended, isDone = false)
      LinkConfirmation.FAILED -> current.copy(maybeError = R.string.error_generic)
    }.copy(isConfirming = false)
  }

  /** The user left the reviewed approval (or closed its confirmation). */
  fun closeLinkReview() {
    val current = review.value ?: return
    review.value = null
    promptFailure.value = null
    if (!current.isDone) cancelLinkApproval()
  }

  /** New device: asks the account's other devices to approve this one (the server then holds the code's key). */
  suspend fun askOtherDevices(): Result<Unit> =
    runCatching { graph.core.core().announceDeviceLink() }
      .onFailure { Log.w(TAG, "asking the other devices failed", it) }

  /** Existing device: joins the new device whose QR code was scanned or typed. */
  suspend fun approveLink(code: String): Result<Unit> =
    runCatching { graph.core.core().approveDeviceLink(code.trim()) }
      .onFailure { Log.w(TAG, "joining the new device failed", it) }

  /** Checks the code the user typed from the new device's screen; sends the keys if it is right. */
  suspend fun confirmLink(typedCode: String): LinkConfirmation =
    try {
      graph.core.core().confirmDeviceLink(typedCode.trim())
      LinkConfirmation.CONFIRMED
    } catch (failure: TeamCraftException.LinkCodeRejected) {
      LinkConfirmation.WRONG_CODE
    } catch (failure: TeamCraftException.LinkAttemptEnded) {
      LinkConfirmation.ATTEMPT_ENDED
    } catch (failure: TeamCraftException) {
      Log.w(TAG, "sending the keys to the new device failed", failure)
      LinkConfirmation.FAILED
    }

  /** The user left the code step: nothing is sent, syncing resumes. */
  fun cancelLinkApproval() {
    graph.scope.launch { runCatching { graph.core.core().cancelDeviceLinkApproval() } }
  }

  private suspend fun endSession() {
    // Wake-ups that arrive from now on show nothing; notifications are cleared last, when no
    // core can be opened any more to post new ones.
    graph.push.beginSignOut()
    linkRequestList.value = emptyList()
    forgetSession()
    withContext(Dispatchers.IO) { startCache.delete() }
    review.value = null
    // A recording and its take don't outlive the account on this device (#204).
    voicePlayer.stop()
    voice.forgetAll()
    graph.calls.hangUp()
    graph.push.unregister()
    // A setup given up on leaves no keys behind, and its device is revoked (issue #415).
    runCatching { graph.core.core().forgetUnfinishedSetup() }
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
      status = answer.status.toUShort(),
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
      // The sign-in screen showed the offline fallback: ask for the server's own page again.
      if (isOnline && state.value is AppState.SignedOut && !isPageFresh) launch { refreshSignInPage() }
      val current = state.value as? AppState.NeedsServer ?: return@collect
      val maybeError = current.maybeError
      if (isOnline && !busy.value && maybeError != null && isRetryableWhenOnline(maybeError)) {
        submitServer(current.prefill)
      }
    }
  }

  /**
   * The app shows what the store holds before the core's client opened. Should that find the store
   * unreadable after all, the core says so on the next change (issue #414).
   */
  private fun leaveWhenStoreTurnsUnreadable() = viewModelScope.launch {
    changeCount.collect {
      if (state.value !is AppState.Ready) return@collect
      val maybeAccount = runCatching { graph.core.core().accountState() }.getOrNull()
      if (maybeAccount is AccountState.StoreUnreadable) state.value = AppState.StoreUnreadable(maybeAccount.problem)
    }
  }

  /** Asks the server for its sign-in page whenever the sign-in screen appears. */
  private fun loadSignInPageWhenShown() = viewModelScope.launch {
    state.map { it is AppState.SignedOut }.distinctUntilChanged().collect { isShown ->
      if (isShown) launch { refreshSignInPage() }
    }
  }

  /**
   * Fetches `GET /api/v1/server-info` of the active server and shows its sign-in page. When the
   * server can't be reached the last page of this server stays, or else "Sign in with <brand>".
   */
  private suspend fun refreshSignInPage() {
    val server = activeServer.value ?: return
    val origin = server.address.origin
    val isJustFetched = maybePageOrigin == origin && isPageFresh &&
      SystemClock.elapsedRealtime() - pageFetchedAtMillis < PAGE_REUSE_MILLIS
    if (isJustFetched) return
    val brandName = brand.value?.displayName ?: server.displayName
    val maybeFresh = try {
      val answer = ServerInfoClient.fetch(server.address.serverInfoUrl)
      if (answer.status != 200) {
        null
      } else {
        withContext(Dispatchers.Default) {
          signInPage(answer.body.decodeToString(), origin, brandName, Locale.getDefault().toLanguageTag())
        }
      }
    } catch (failure: IOException) {
      null
    }
    if (activeServer.value?.address?.origin != origin) return
    when {
      maybeFresh != null -> showSignInPage(origin, maybeFresh, isFresh = true)
      maybePageOrigin != origin -> showSignInPage(origin, ssoOnlySignInPage(brandName), isFresh = false)
      else -> isPageFresh = false
    }
  }

  /** Shows [info] for the server at [origin]; its provider icon is cached by the brand store. */
  private fun showSignInPage(origin: String, info: SignInPageInfo, isFresh: Boolean) {
    page.value = info
    maybePageOrigin = origin
    isPageFresh = isFresh
    if (isFresh) pageFetchedAtMillis = SystemClock.elapsedRealtime()
    // The offline fallback names the brand, not a provider: no provider icon next to it.
    val maybeStore = maybeBrandStore?.takeIf { it.origin == origin && info.maybeSsoIconUrl != null }
    if (maybeStore == null) {
      pageIcon.value = null
      // The server dropped its icon: forget the cached one.
      if (isFresh) maybeBrandStore?.takeIf { it.origin == origin }?.let { viewModelScope.launch { it.refreshSsoIcon(null) } }
      return
    }
    val store = maybeStore
    viewModelScope.launch {
      val maybeCached = withContext(Dispatchers.IO) { store.ssoIcon() }
      if (maybePageOrigin == origin) pageIcon.value = maybeCached
      store.refreshSsoIcon(info.maybeSsoIconUrl)
      val maybeFresh = withContext(Dispatchers.IO) { store.ssoIcon() }
      if (maybePageOrigin == origin) pageIcon.value = maybeFresh
    }
  }

  /** The confirmation screen shows the brand the server announced; logos follow when fetched. */
  private fun previewBrand(checked: CheckedServer) {
    val address = checked.server.address
    val store = BrandStore(getApplication(), address.origin, address.storageKey)
    maybeBrandStore = store
    brand.value = checked.maybeBrand
    icons.value = null
    viewModelScope.launch {
      val maybeFresh = store.refresh()
      if (maybeBrandStore === store) brand.value = maybeFresh ?: checked.maybeBrand
      loadIcons(store)
    }
  }

  /** The active server's brand: the cached one at once, then the server's answer. */
  private fun showServer(server: StoredServer) {
    activeServer.value = server
    val address = server.address
    if (maybeBrandStore?.let { it.origin == address.origin } != true) {
      maybeBrandStore = BrandStore(getApplication(), address.origin, address.storageKey)
      brand.value = null
      icons.value = null
    }
    val store = maybeBrandStore ?: return
    viewModelScope.launch {
      val maybeCached = withContext(Dispatchers.IO) { store.cached() }
      if (maybeBrandStore === store && maybeCached != null) brand.value = maybeCached
      loadIcons(store)
      val maybeFresh = store.refresh()
      if (maybeBrandStore === store) brand.value = maybeFresh
      loadIcons(store)
    }
  }

  /** The icons [store] has cached; after a refresh they may be new even when the brand is not. */
  private suspend fun loadIcons(store: BrandStore) {
    val loaded = withContext(Dispatchers.IO) { BrandIcons(store.icon(isDark = false), store.icon(isDark = true)) }
    if (maybeBrandStore === store) icons.value = loaded
  }

  private fun forgetActiveServer() {
    graph.auth.activate(null)
    // The next server's crash reports setting comes after its sign-in.
    graph.crashReports.forgetServer()
    activeServer.value = null
    maybeBrandStore = null
    brand.value = null
    icons.value = null
    page.value = null
    pageIcon.value = null
    maybePageOrigin = null
    isPageFresh = false
  }

  private suspend fun loadAccountState(): AppState =
    try {
      when (val account = graph.core.core().accountState()) {
        is AccountState.NeedsEnrollment -> AppState.NeedsEnrollment(account.hasExistingIdentity)
        is AccountState.Ready -> ready(account)
        is AccountState.StoreUnreadable -> AppState.StoreUnreadable(account.problem)
      }
    } catch (failure: TeamCraftException.SignedOut) {
      graph.auth.signOut()
      AppState.SignedOut(R.string.error_signed_out)
    } catch (failure: TeamCraftException) {
      AppState.Failed(errorText(failure))
    }

  private suspend fun ready(account: AccountState): AppState {
    val ready = account as? AccountState.Ready ?: return AppState.NeedsEnrollment(hasExistingIdentity = true)
    // Before anything of the app shows: a share left from a session that ran out isn't for somebody
    // else signing in now.
    if (withContext(Dispatchers.IO) { shares.inbox.claim(ready.userId) }) {
      shareOfOtherAccount.value = true
      // The folder of one on its way into a composer went too.
      shares.forgetHandedOver()
      PendingShare.current.value = null
      refreshShare()
    }
    // Also started on every foreground, which keeps trying and tells the user (issue #1142).
    runCatching { graph.core.core().start() }
      .onFailure { Log.w(TAG, "sync not started: ${it.javaClass.simpleName}") }
    // The call listener too: right after the first setup nothing else starts it until the app
    // comes to the foreground again, and calls in the meantime didn't ring.
    runCatching { graph.calls.start() }
      .onFailure { Log.w(TAG, "calls not started: ${it.javaClass.simpleName}") }
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

    /** The core's logcat tag too (issue #1067): `adb logcat -s TeamCraft` shows both. */
    private const val TAG = "TeamCraft"

    /** How long the chat list must stay unchanged before it is kept for the next start. */
    private const val START_CACHE_SETTLE_MILLIS = 1_000L

    /** Read-ahead passes in a row before it waits for the next list change. */
    private const val READ_AHEAD_PASSES = 2

    /** A sign-in page this young (just fetched with the server check) is not asked for again. */
    private const val PAGE_REUSE_MILLIS = 10_000L

    /** Plain HTTP to other hosts than the device itself only in development builds. */
    private val ALLOW_INSECURE_HTTP = BuildConfig.DEBUG

    @StringRes
    fun errorText(failure: TeamCraftException): Int = when (failure) {
      is TeamCraftException.Network -> R.string.error_network
      is TeamCraftException.SignedOut -> R.string.error_signed_out
      is TeamCraftException.WrongPassphrase -> R.string.error_wrong_passphrase
      is TeamCraftException.BackupOutOfMemory -> R.string.error_backup_out_of_memory
      is TeamCraftException.BackupTooNew -> R.string.error_backup_too_new
      is TeamCraftException.BackupDamaged -> R.string.error_backup_damaged
      is TeamCraftException.Storage -> R.string.error_storage
      is TeamCraftException.CallMoveRefused -> R.string.error_call_move_refused
      is TeamCraftException.CallEncryptionPending -> R.string.error_call_encryption_pending
      is TeamCraftException.CallDeviceLimit -> R.string.error_call_device_limit
      is TeamCraftException.TooLarge -> R.string.error_too_large
      is TeamCraftException.LinkCodeInvalid -> R.string.link_code_invalid
      is TeamCraftException.LinkCodeForNewDevice -> R.string.link_code_for_new_device
      is TeamCraftException.LinkCodeForSignedInDevice -> R.string.link_code_for_signed_in_device
      is TeamCraftException.LinkCodeExpired -> R.string.link_code_expired
      is TeamCraftException.BugReportNoRecipients -> R.string.report_error_no_recipients
      is TeamCraftException.BugReportQueueFull -> R.string.report_error_queue_full
      else -> R.string.error_generic
    }
  }

  override fun onCleared() {
    ProcessLifecycleOwner.get().lifecycle.removeObserver(startCacheOnLeave)
    voice.close()
    voicePlayer.release()
    super.onCleared()
  }
}

/**
 * Largest file the app copies in for sending. The server enforces its own (smaller) limit; this
 * only keeps a hostile or endless provider from filling the device.
 */
internal const val MAX_IMPORT_BYTES = 4L * 1024 * 1024 * 1024

/** Copies `input` to `output`; false (and stops) once more than `maxBytes` arrived or on [isCancelled]. */
internal fun copyAtMost(
  input: InputStream,
  output: OutputStream,
  maxBytes: Long,
  isCancelled: () -> Boolean = { false },
): Boolean {
  val buffer = ByteArray(64 * 1024)
  var total = 0L
  while (true) {
    if (isCancelled()) return false
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

/** How many removed or sent draft files the composers keep hidden ([MainViewModel.goneDraftFiles]). */
private const val MAX_GONE_DRAFT_FILES = 256
private const val BIDI_CONTROLS = "\u061C\u200E\u200F\u202A\u202B\u202C\u202D\u202E\u2066\u2067\u2068\u2069"

/** A page of a server list, or why it could not be read. */
sealed interface PageAnswer<out T> {
  data class Loaded<T>(val value: T) : PageAnswer<T>

  data class Failed(val failure: LoadFailure) : PageAnswer<Nothing>
}

/** The server's answer to an action, or the text of why there was none. */
sealed interface ActionAnswer<out T> {
  data class Answered<T>(val value: T) : ActionAnswer<T>

  data class Failed(@StringRes val text: Int) : ActionAnswer<Nothing>
}

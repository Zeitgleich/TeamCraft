package de.zeitgleich.teamcraft.notifications

import android.Manifest
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.util.Log
import androidx.annotation.StringRes
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.CoreRepository
import de.zeitgleich.teamcraft.security.AppLockPrefs
import de.zeitgleich.teamcraft.ui.MainActivity
import de.zeitgleich.teamcraft.ui.MainViewModel
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.FlowPreview
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.debounce
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.filterNotNull
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.receiveAsFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import uniffi.teamcraft_ffi.CallStateInfo
import uniffi.teamcraft_ffi.LinkArrival
import uniffi.teamcraft_ffi.TeamCraftException

/**
 * The meeting watch while the process lives (#2258), as desktop keeps one beside its reminders:
 * the core's `meetingWatch` runs once per foreground, whenever the user's meetings change (a meeting
 * changed, somebody knocked, a newly verified admitters list), and after joining a call. Each run
 * keeps the organizer's upkeep current and lets in, by itself, whoever came with the meeting link
 * where this device may (the organizer's "no lobby", or the organizer's signed admitters while in
 * the meeting's call; docs/meetings.md, "Lobby"). The core decides who; this only says it, with
 * "Remove" ([LinkArrivalNotifier]).
 *
 * The meetings screen reads its list again on [meetingsChanges] instead of on every change of the
 * store. Runs one after the other; a run asked for while one is under way runs once afterwards.
 */
class MeetingWatch(
  private val core: CoreRepository,
  private val notifier: LinkArrivalNotifier,
  private val callState: StateFlow<CallStateInfo?>,
  private val scope: CoroutineScope,
) {
  private val runs = Channel<Unit>(Channel.CONFLATED)
  private val changes = MutableStateFlow(0L)
  private var lastRevision: ULong? = null

  /** The server told the app has its admission by itself only after this went through once. */
  @Volatile private var isAdmitterMarked = false

  /** Moves when the meetings changed or the app came to the foreground: the meeting screens read again. */
  val meetingsChanges: StateFlow<Long> = changes

  @OptIn(FlowPreview::class)
  fun start() {
    scope.launch { runs.receiveAsFlow().collect { run() } }
    // The revision is a cheap read; the list only when it moved.
    scope.launch {
      core.changeCount.debounce(DEBOUNCE_MS).collect {
        val revision = try {
          core.core().meetingsRevision()
        } catch (error: TeamCraftException) {
          return@collect
        } catch (error: IllegalStateException) {
          // Signed out: nothing to watch.
          if (error is CancellationException) throw error
          return@collect
        }
        if (revision == lastRevision) return@collect
        val isFirst = lastRevision == null
        lastRevision = revision
        // The first read only sets the mark: the foreground already asked for a run.
        if (!isFirst) meetingsChanged()
      }
    }
    // Joining a call: in somebody else's meeting this device may be an admitter now.
    scope.launch {
      callState.map { it?.conversationId }.distinctUntilChanged().filterNotNull().collect { request() }
    }
  }

  /** The app came to the foreground: what happened meanwhile is read once. */
  fun appForeground() = meetingsChanged()

  private fun meetingsChanged() {
    changes.update { it + 1 }
    request()
  }

  private fun request() {
    runs.trySend(Unit)
  }

  private suspend fun run() {
    try {
      val chat = core.core()
      if (!isAdmitterMarked) {
        chat.markAdmitter()
        isAdmitterMarked = true
      }
      chat.meetingWatch().forEach(notifier::show)
    } catch (error: TeamCraftException) {
      // Offline or refused: the next change or foreground tries again.
      Log.i(TAG, "meeting watch: ${error.javaClass.simpleName}")
    } catch (error: IllegalStateException) {
      if (error is CancellationException) throw error
    }
  }

  private companion object {
    const val DEBOUNCE_MS = 500L
    const val TAG = "TeamCraftMeetings"
  }
}

/**
 * Says that somebody was let into a meeting by this device without a click (#2258): one
 * notification per person and meeting, with "Remove" (as desktop's notice has it), which takes them
 * out of the meeting chat and its encryption. Follows the content setting and the app lock as
 * message notifications do: the lock screen says only that somebody joined.
 */
class LinkArrivalNotifier(
  private val context: Context,
  private val contentPrefs: NotificationContentPrefs,
) {
  private val manager = NotificationManagerCompat.from(context)

  /** Shows [arrival]; [maybeProblem] says why "Remove" didn't go through. */
  fun show(arrival: LinkArrival, @StringRes maybeProblem: Int? = null) {
    if (!canPost()) return
    val privacy = MessagePrivacy.of(contentPrefs.content.value, isAppLockOn(), isVerified = true)
    val publicTitle = context.getString(R.string.link_arrival_hidden_title)
    val publicText = context.getString(R.string.link_arrival_hidden_text)
    val title = if (privacy.showsNames) context.getString(R.string.link_arrival_title, arrival.displayName) else publicTitle
    val text = when {
      maybeProblem != null -> context.getString(maybeProblem)
      privacy.showsNames -> context.getString(R.string.link_arrival_text, arrival.meetingTitle)
      else -> publicText
    }
    val builder = NotificationCompat.Builder(context, NotificationChannels.MEETINGS)
      .setSmallIcon(R.drawable.ic_notification)
      .setCategory(NotificationCompat.CATEGORY_EVENT)
      .setContentTitle(title)
      .setContentText(text)
      .setStyle(NotificationCompat.BigTextStyle().bigText(text))
      .setOnlyAlertOnce(true)
      .setSilent(maybeProblem != null)
      .setAutoCancel(true)
      .setContentIntent(openIntent(arrival.conversationId))
      .setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
      .setPublicVersion(
        NotificationCompat.Builder(context, NotificationChannels.MEETINGS)
          .setSmallIcon(R.drawable.ic_notification)
          .setContentTitle(publicTitle)
          .setContentText(publicText)
          .build(),
      )
    if (privacy.offersActions) builder.addAction(removeAction(arrival))
    try {
      manager.notify(tag(arrival), LINK_ARRIVAL_ID, builder.build())
    } catch (denied: SecurityException) {
      // The permission was revoked between the check and the post.
      Log.w(TAG, "link arrival notification not posted: permission missing")
    }
  }

  /**
   * "Remove" pressed (from [NotificationActionReceiver]): takes the person out through the core and
   * withdraws the notification, or shows it again with what went wrong, so a failure never passes
   * silently.
   */
  suspend fun remove(core: CoreRepository, arrival: LinkArrival) {
    val maybeProblem = try {
      core.core().removeLinkArrival(arrival.conversationId, arrival.userId)
      null
    } catch (failure: TeamCraftException) {
      if (failure is TeamCraftException.Network) MainViewModel.errorText(failure) else R.string.link_arrival_remove_failed
    } catch (failure: CancellationException) {
      throw failure
    } catch (failure: IllegalStateException) {
      // Signed out meanwhile: nothing to remove from any more.
      null
    }
    if (maybeProblem == null) manager.cancel(tag(arrival), LINK_ARRIVAL_ID) else show(arrival, maybeProblem)
  }

  private fun removeAction(arrival: LinkArrival): NotificationCompat.Action {
    val intent = NotificationActionReceiver.linkArrivalIntent(context, arrival)
      // Each arrival's intent differs in its data URI, so their PendingIntents never collide.
      .setData(Uri.fromParts(URI_SCHEME, tag(arrival), null))
    val pending = PendingIntent.getBroadcast(
      context,
      0,
      intent,
      PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
    )
    return NotificationCompat.Action.Builder(R.drawable.ic_notification, context.getString(R.string.link_arrival_remove), pending)
      .setShowsUserInterface(false)
      // Changing who is in a meeting needs the unlocked phone (Android 12+).
      .setAuthenticationRequired(true)
      .build()
  }

  /** Opens the meeting's chat. */
  private fun openIntent(conversationId: String): PendingIntent {
    val intent = Intent(context, MainActivity::class.java)
      .putExtra(MainActivity.EXTRA_CONVERSATION_ID, conversationId)
      .putExtra(MainActivity.EXTRA_LAUNCH_TOKEN, LaunchToken.of(context))
      .setData(Uri.fromParts(URI_SCHEME, conversationId, null))
      .addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP)
    return PendingIntent.getActivity(context, 0, intent, PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE)
  }

  private fun isAppLockOn(): Boolean = AppLockPrefs.prefs(context).isEnabled

  private fun canPost(): Boolean =
    Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU ||
      ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED

  private companion object {
    /** Told apart by their tag (one per meeting and person); the ID is fixed. */
    const val LINK_ARRIVAL_ID = 6
    const val URI_SCHEME = "teamcraft-link-arrival"
    const val TAG = "TeamCraftNotify"

    fun tag(arrival: LinkArrival): String = "link-arrival:${arrival.meetingId}:${arrival.userId}"
  }
}

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
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.FlowPreview
import kotlinx.coroutines.flow.debounce
import kotlinx.coroutines.flow.drop
import kotlinx.coroutines.launch
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.CoreRepository
import de.zeitgleich.teamcraft.security.AppLockPrefs
import de.zeitgleich.teamcraft.ui.JoinStep
import de.zeitgleich.teamcraft.ui.joinOutcomeMessage
import de.zeitgleich.teamcraft.ui.MainActivity
import de.zeitgleich.teamcraft.ui.MainViewModel
import uniffi.teamcraft_ffi.JoinHistory
import uniffi.teamcraft_ffi.JoinRequestItem
import uniffi.teamcraft_ffi.JoinRequestOutcome
import uniffi.teamcraft_ffi.JoinRequestStanding
import uniffi.teamcraft_ffi.TeamCraftException

/**
 * Notifications for admins' requests to join a channel the user owns (admin.md §2.4.1, issue
 * #799): one per request (tagged like the core says, so a newer request of the same admin for the
 * same channel replaces the older one's), with Approve where the verified roster allows it and
 * Decline; withdrawn once the request is gone. Posted while the app runs ([JoinRequestWatcher]): no
 * push wakes the phone for a request yet, the list is read again after every connection.
 */
class JoinRequestNotifier(
  private val context: Context,
  private val contentPrefs: NotificationContentPrefs,
) {
  private val manager = NotificationManagerCompat.from(context)

  /** Shows [request]; [maybeProblem] says why a decision from the notification didn't go through. */
  fun show(request: JoinRequestItem, @StringRes maybeProblem: Int? = null) {
    if (!canPost()) return
    val privacy = MessagePrivacy.of(contentPrefs.content.value, isAppLockOn(), isVerified = true)
    val spec = JoinRequestNotifications.spec(request.standing, privacy)
    val publicTitle = context.getString(R.string.join_request_notification_hidden_title)
    val publicText = context.getString(R.string.join_request_notification_hidden_text)
    val title = if (spec.showsNames) title(request) else publicTitle
    val text = when {
      maybeProblem != null -> context.getString(maybeProblem)
      !spec.showsNames -> publicText
      request.standing == JoinRequestStanding.APPROVABLE -> context.getString(R.string.join_request_notification_text)
      else -> context.getString(R.string.join_request_not_admin)
    }
    val builder = NotificationCompat.Builder(context, NotificationChannels.JOIN_REQUESTS)
      .setSmallIcon(R.drawable.ic_notification)
      .setCategory(NotificationCompat.CATEGORY_SOCIAL)
      .setContentTitle(title)
      .setContentText(text)
      .setStyle(NotificationCompat.BigTextStyle().bigText(text))
      .setWhen(request.requestedAtMs)
      .setShowWhen(true)
      // A problem replaces the notification quietly: the user just acted on it.
      .setOnlyAlertOnce(true)
      .setSilent(maybeProblem != null)
      .setContentIntent(openIntent(request.conversationId))
      // The lock screen says that somebody asks, not who or for which channel.
      .setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
      .setPublicVersion(
        NotificationCompat.Builder(context, NotificationChannels.JOIN_REQUESTS)
          .setSmallIcon(R.drawable.ic_notification)
          .setContentTitle(publicTitle)
          .setContentText(publicText)
          .build(),
      )
    spec.actions.forEach { action -> builder.addAction(action(action, request)) }
    try {
      manager.notify(request.notificationTag, JOIN_REQUEST_ID, builder.build())
    } catch (denied: SecurityException) {
      // The permission was revoked between the check and the post.
      Log.w(TAG, "join request notification not posted: permission missing")
    }
  }

  fun withdraw(tag: String) {
    manager.cancel(tag, JOIN_REQUEST_ID)
  }

  /**
   * Approve or Decline pressed in the notification (from [NotificationActionReceiver]): runs the
   * decision through the core and withdraws the notification, or shows it again with what went
   * wrong, so a failure never passes silently. Approve here never shares the earlier history (the
   * text says so); that choice is in the app.
   */
  suspend fun decide(core: CoreRepository, step: JoinStep, conversationId: String, userId: String, tag: String) {
    val maybeProblem = try {
      when (val outcome = step.run(core, conversationId, userId, JoinHistory.FROM_JOIN)) {
        JoinRequestOutcome.DONE, JoinRequestOutcome.GONE -> null
        else -> joinOutcomeMessage(step, outcome)
      }
    } catch (failure: TeamCraftException) {
      MainViewModel.errorText(failure)
    } catch (failure: CancellationException) {
      throw failure
    } catch (failure: IllegalStateException) {
      // Signed out meanwhile: nothing to decide for any more.
      null
    }
    if (maybeProblem == null) {
      withdraw(tag)
      return
    }
    // Still open: shown again with the problem, so Approve or Decline can be tried again.
    val maybeRequest = runCatching { core.core().joinRequests() }.getOrNull()?.firstOrNull { it.notificationTag == tag }
    if (maybeRequest == null) withdraw(tag) else show(maybeRequest, maybeProblem)
  }

  /** The admin with their username (two people may share a name) and the channel. */
  private fun title(request: JoinRequestItem): String =
    if (request.conversationName.isEmpty()) {
      context.getString(R.string.join_request_notification_title_unnamed, request.userLabel)
    } else {
      context.getString(R.string.join_request_notification_title, request.userLabel, "#" + request.conversationName)
    }

  private fun action(action: JoinRequestNotificationAction, request: JoinRequestItem): NotificationCompat.Action {
    val (receiverAction, label) = when (action) {
      JoinRequestNotificationAction.APPROVE -> NotificationActionReceiver.ACTION_APPROVE_JOIN to R.string.join_request_approve
      JoinRequestNotificationAction.DECLINE -> NotificationActionReceiver.ACTION_DECLINE_JOIN to R.string.join_request_decline
    }
    val intent = NotificationActionReceiver.joinRequestIntent(context, receiverAction, request)
      // Each request's intents differ in their data URI, so their PendingIntents never collide.
      .setData(Uri.fromParts(URI_SCHEME, request.notificationTag, null))
    val pending = PendingIntent.getBroadcast(
      context,
      0,
      intent,
      PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
    )
    return NotificationCompat.Action.Builder(R.drawable.ic_notification, context.getString(label), pending)
      .setShowsUserInterface(false)
      // Letting somebody into a channel needs the unlocked phone (Android 12+).
      .setAuthenticationRequired(true)
      .build()
  }

  /** Opens the channel, where the request waits above the messages. */
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
    /** Told apart by their tag (the core's, one per admin and channel); the ID is fixed. */
    const val JOIN_REQUEST_ID = 4
    const val URI_SCHEME = "teamcraft-join-request"
    const val TAG = "TeamCraftNotify"
  }
}

/**
 * While the process lives, shows a notification for every new request and withdraws the ones that
 * are gone, as the core reports them on a change. The core remembers what this device notified
 * about, so a restart doesn't notify again.
 */
class JoinRequestWatcher(
  private val core: CoreRepository,
  private val notifier: JoinRequestNotifier,
  private val scope: CoroutineScope,
) {
  @OptIn(FlowPreview::class)
  fun start() {
    scope.launch {
      core.changeCount.drop(1).debounce(DEBOUNCE_MS).collect { check() }
    }
  }

  private suspend fun check() {
    // A failed read changes nothing; the next change asks again.
    val changes = runCatching { core.core().joinRequestAlerts() }.getOrNull() ?: return
    changes.goneTags.forEach(notifier::withdraw)
    changes.new.forEach { notifier.show(it) }
  }

  private companion object {
    const val DEBOUNCE_MS = 500L
  }
}

/** A button of a join request's notification. */
enum class JoinRequestNotificationAction { APPROVE, DECLINE }

/** What a join request's notification shows and offers. */
data class JoinRequestNotificationSpec(val showsNames: Boolean, val actions: List<JoinRequestNotificationAction>)

/** Pure, unit-tested: the notification follows the content setting and the app lock like messages do. */
object JoinRequestNotifications {
  /**
   * Names only where the setting shows names; actions only where they are allowed (not behind the
   * app lock, not without knowing who asks) and Approve only for an admin of the fresh roster.
   */
  fun spec(standing: JoinRequestStanding, privacy: MessagePrivacy): JoinRequestNotificationSpec {
    val actions = when {
      !privacy.offersActions -> emptyList()
      standing == JoinRequestStanding.APPROVABLE ->
        listOf(JoinRequestNotificationAction.DECLINE, JoinRequestNotificationAction.APPROVE)
      else -> listOf(JoinRequestNotificationAction.DECLINE)
    }
    return JoinRequestNotificationSpec(showsNames = privacy.showsNames, actions = actions)
  }
}

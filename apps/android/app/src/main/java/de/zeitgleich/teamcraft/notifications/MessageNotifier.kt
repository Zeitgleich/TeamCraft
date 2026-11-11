package de.zeitgleich.teamcraft.notifications

import android.Manifest
import android.app.Notification
import android.app.NotificationManager
import android.app.PendingIntent
import de.zeitgleich.teamcraft.calls.CallActivity
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.Typeface
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.SystemClock
import android.text.SpannableString
import android.text.Spanned
import android.text.style.StyleSpan
import android.util.Log
import java.util.Base64
import java.util.concurrent.atomic.AtomicLong
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.app.Person
import androidx.core.app.RemoteInput
import androidx.core.content.ContextCompat
import androidx.core.content.LocusIdCompat
import androidx.core.content.pm.ShortcutInfoCompat
import androidx.core.content.pm.ShortcutManagerCompat
import androidx.core.graphics.drawable.IconCompat
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.security.AppLockPrefs
import de.zeitgleich.teamcraft.ui.BubbleActivity
import de.zeitgleich.teamcraft.ui.MainActivity
import de.zeitgleich.teamcraft.ui.ShareShortcuts
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import uniffi.teamcraft_ffi.NotificationAttachmentKind
import uniffi.teamcraft_ffi.NotificationGap
import uniffi.teamcraft_ffi.PushNotification
import uniffi.teamcraft_ffi.PushNotificationKind

/**
 * Message notifications: one MessagingStyle notification per conversation (tag = conversation ID),
 * shown by Android as a conversation through a long-lived shortcut, grouped under a summary once two
 * or more conversations show, with "reply" and "mark as read" actions that work without opening the
 * app (Android Auto and Wear OS need them). A message is posted once, after its wake-up was opened;
 * every later change replaces the notification under the same ID without alerting again, so a watch
 * sees one notification per message (issue #739). A message not read yet is posted only once its
 * text is read, or as a placeholder on the phone only when that takes too long (notifications.md,
 * "Watches, bands and Android Auto"). What it shows is [MessageNotificationSpecs]; the lines survive a process restart
 * ([NotificationStore]).
 */
class MessageNotifier(
  private val context: Context,
  private val contentPrefs: NotificationContentPrefs,
  private val avatars: AvatarCache,
  private val store: NotificationStore = NotificationStore(context),
  /** Whether the phone projects to a car now (Android Auto). */
  private val isInCar: () -> Boolean = { false },
  private val elapsedNow: () -> Long = SystemClock::elapsedRealtime,
  /** Conversations the user opted in to show as bubbles (#591). */
  private val bubbles: BubblePrefs = BubblePrefs(context),
) {
  private val manager = NotificationManagerCompat.from(context)
  private val lock = Any()
  private var maybeShown: ShownNotifications? = null

  /** The summary last posted, so an unchanged one is not posted (and mirrored) again. */
  private var maybeLastSummary: SummarySpec? = null

  /** Rises with every post; from the clock, so a new process stays above the old one's. */
  private val revisions = AtomicLong(System.currentTimeMillis())
  private val confirms = HashMap<String, PostConfirm>()
  private val confirmJobs = HashMap<String, Job>()

  /** When the last quiet note went up, so a burst shows one. */
  private var maybeLastQuietNoteAtMs: Long? = null
  private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

  init {
    NotificationChannels.create(context)
  }

  /**
   * Adds a pushed message to its conversation's notification (see [PushDecision.ShowMessage]). A
   * placeholder comes only after the wait for the text ([PushRetry.SETTLE_BOUND_MS]) and alerts at
   * once; `payload` is kept with it to read it later. Returns whether a notification for it is up; a
   * repeat of a wake-up whose line went (read, swiped away) doesn't bring it back.
   */
  fun show(notification: PushNotification, isAlerting: Boolean, payload: ByteArray? = null): Boolean {
    val line = line(notification, payload)
    synchronized(lock) {
      val shown = shown()
      val previous = shown.conversations[notification.conversationId]
      if (NotificationLines.isGone(previous?.lines.orEmpty(), line, notification.isDuplicate)) return false
      val entry = entry(notification, previous, NotificationLines.withWakeUp(previous?.lines.orEmpty(), line, notification.isDuplicate))
      val badge = notification.maybeBadge?.toInt() ?: shown.maybeBadge
      update(shown.copy(conversations = shown.conversations + (entry.conversationId to entry), maybeBadge = badge))
      val alerting = MessageNotificationSpecs.alerting(
        isAlerting = isAlerting,
        maybePrevious = previous,
        entry = entry,
        nowMs = elapsedNow(),
        isInCar = isInCar(),
        isSenderSilent = notification.isSilent,
      )
      post(entry, alerting)
      postSummary()
      return true
    }
  }

  /**
   * A wake-up that could not be opened before opened now ([showUnreadable] said "New message" or
   * nothing): the real notification takes over. It alerts only if nothing did for it yet;
   * otherwise it is new to bridges and silent on the phone.
   */
  fun showOpenedLate(notification: PushNotification, isAlerting: Boolean, payload: ByteArray) {
    val line = line(notification, payload)
    synchronized(lock) {
      val shown = shown()
      val previous = shown.conversations[notification.conversationId]
      val entry = entry(notification, previous, NotificationLines.merge(previous?.lines.orEmpty(), line))
      val badge = notification.maybeBadge?.toInt() ?: shown.maybeBadge
      update(shown.copy(conversations = shown.conversations + (entry.conversationId to entry), maybeBadge = badge))
      val alerting = when {
        isAlerting -> NotificationAlerting.ALERT
        line.isPlaceholder -> NotificationAlerting.SILENT
        else -> NotificationAlerting.REFRESH
      }
      post(entry, alerting)
      manager.cancel(UNREADABLE_TAG, OTHER_ID)
      postSummary()
    }
  }

  /**
   * A later look read a message that was shown as a placeholder ([PushRetry]): fills in its line
   * under the same ID. It alerts if the placeholder had not (one alert, with the text, everywhere);
   * otherwise silently on the phone but mirrored again to watches and bands
   * ([NotificationAlerting.REFRESH]). Does nothing when the notification is gone or already
   * complete. `isSuppressed`: muted or DND by now, so nothing but the text changes. Carries what only
   * the text could tell: a keyword mention, a message the sender sent silently. Returns whether it
   * filled anything in.
   */
  fun fillIn(notification: PushNotification, isSuppressed: Boolean = false): Boolean {
    val line = line(notification, null)
    synchronized(lock) {
      val shown = shown()
      val previous = shown.conversations[notification.conversationId] ?: return false
      val lines = NotificationLines.filledIn(previous.lines, line) ?: return false
      val entry = previous.copy(
        maybeTitle = title(notification) ?: previous.maybeTitle,
        isVerified = previous.isVerified || notification.isConversationVerified,
        isMention = previous.isMention || notification.isMention || notification.isUrgent,
        lines = lines,
      )
      update(shown.copy(conversations = shown.conversations + (entry.conversationId to entry)))
      val alerting = MessageNotificationSpecs.alerting(
        isAlerting = false,
        maybePrevious = previous,
        entry = entry,
        nowMs = elapsedNow(),
        isInCar = isInCar(),
        isSenderSilent = notification.isSilent,
        isSuppressed = isSuppressed,
      )
      post(entry, alerting)
      postSummary()
      return true
    }
  }

  /**
   * A wake-up that was no message (a reaction, an edit): takes away a placeholder that stood for
   * it, and otherwise re-posts the conversation's notification silently. Returns whether a
   * notification of this conversation was there to update.
   */
  fun applyHiddenChange(notification: PushNotification): Boolean {
    synchronized(lock) {
      val shown = shown()
      val withBadge = notification.maybeBadge?.let { shown.copy(maybeBadge = it.toInt()) } ?: shown
      val entry = withBadge.conversations[notification.conversationId]
      if (entry == null) {
        update(withBadge)
        return false
      }
      val lines = notification.maybeMessageId?.let { NotificationLines.withoutPlaceholder(entry.lines, it) } ?: entry.lines
      if (lines.isEmpty()) {
        update(withBadge.copy(conversations = withBadge.conversations - entry.conversationId))
        manager.cancel(entry.conversationId, MESSAGE_ID)
      } else {
        val updated = entry.copy(lines = lines)
        update(withBadge.copy(conversations = withBadge.conversations + (entry.conversationId to updated)))
        post(updated, NotificationAlerting.SILENT)
      }
      postSummary()
      return true
    }
  }

  /**
   * The message was read before this wake-up got looked at: its line and the ones it read go, as
   * with a `dismiss`. Returns whether a notification of this conversation was there.
   */
  fun applyRead(notification: PushNotification): Boolean {
    val isThere = synchronized(lock) { notification.conversationId in shown().conversations }
    if (isThere) {
      removeRead(notification.conversationId, notification.maybeThreadId, notification.maybeMessageId, notification.maybeBadge)
    }
    return isThere
  }

  /** Ends the hold of the placeholder of this wake-up as `end` says. Returns whether one was held. */
  fun endHold(payload: ByteArray, end: HoldEnd): Boolean {
    val encoded = Base64.getEncoder().encodeToString(payload)
    synchronized(lock) {
      val shown = shown()
      val entry = shown.conversations.values.firstOrNull { entry ->
        entry.lines.any { it.maybePayload == encoded && it.isAlertHeld }
      } ?: return false
      val lines = entry.lines.map { if (it.maybePayload == encoded) it.copy(isAlertHeld = false) else it }
      if (end == HoldEnd.DROP) {
        val id = entry.lines.first { it.maybePayload == encoded }.maybeMessageId
        val left = id?.let { NotificationLines.withoutPlaceholder(lines, it) } ?: lines
        if (left.isEmpty()) {
          update(shown.copy(conversations = shown.conversations - entry.conversationId))
          manager.cancel(entry.conversationId, MESSAGE_ID)
          postSummary()
          return true
        }
        val updated = entry.copy(lines = left)
        update(shown.copy(conversations = shown.conversations + (updated.conversationId to updated)))
        post(updated, NotificationAlerting.SILENT)
        return true
      }
      val updated = entry.copy(lines = lines)
      update(shown.copy(conversations = shown.conversations + (updated.conversationId to updated)))
      post(updated, if (end == HoldEnd.ALERT) NotificationAlerting.ALERT else NotificationAlerting.SILENT)
      return true
    }
  }

  /** The placeholders whose alert is held now. */
  fun heldLines(): List<HeldLine> = synchronized(lock) {
    shown().conversations.values.flatMap { it.lines }
      .filter { it.isAlertHeld }
      .mapNotNull { line ->
        val payload = line.maybePayload ?: return@mapNotNull null
        runCatching { Base64.getDecoder().decode(payload) }.getOrNull()?.let { HeldLine(it, line.maybeArrivedAtMs) }
      }
  }

  /** Whether a placeholder of this wake-up shows now. */
  fun hasPlaceholderFor(payload: ByteArray): Boolean {
    val encoded = Base64.getEncoder().encodeToString(payload)
    return synchronized(lock) {
      shown().conversations.values.any { entry -> entry.lines.any { it.isPlaceholder && it.maybePayload == encoded } }
    }
  }

  /** Whether this notification's message still shows as a placeholder. */
  fun isPlaceholderShowing(notification: PushNotification): Boolean = synchronized(lock) {
    shown().conversations[notification.conversationId]?.lines.orEmpty()
      .any { it.maybeMessageId == notification.maybeMessageId && it.isPlaceholder }
  }

  /** The sealed wake-ups of the placeholders shown now, to look for their messages again. */
  fun placeholderPayloads(): List<ByteArray> = synchronized(lock) {
    shown().conversations.values.flatMap { it.lines }
      .mapNotNull { line -> line.maybePayload?.takeIf { line.isPlaceholder } }
      .mapNotNull { encoded -> runCatching { Base64.getDecoder().decode(encoded) }.getOrNull() }
  }

  /**
   * A high-priority wake-up that turned out to have nothing to announce, and no notification to
   * update: FCM lowers the priority of apps with a pattern of high-priority messages that show
   * nothing, so a short quiet note shows and goes by itself. Never mirrored, no sound, and a burst
   * shows one ([PushRetry.isQuietNoteDue]).
   */
  fun showQuietNote() {
    if (!canPost()) return
    val nowMs = elapsedNow()
    synchronized(lock) {
      if (!PushRetry.isQuietNoteDue(maybeLastQuietNoteAtMs, nowMs)) return
      maybeLastQuietNoteAtMs = nowMs
    }
    val notification = NotificationCompat.Builder(context, NotificationChannels.OTHER)
      .setSmallIcon(R.drawable.ic_notification)
      .setContentTitle(context.getString(R.string.push_fetching))
      .setCategory(NotificationCompat.CATEGORY_STATUS)
      .setSilent(true)
      .setLocalOnly(true)
      .setTimeoutAfter(QUIET_NOTE_MS)
      .setVisibility(NotificationCompat.VISIBILITY_PUBLIC)
      .build()
    postSafely(QUIET_NOTE_TAG, OTHER_ID, notification)
  }

  /**
   * A wake-up that could not be opened at all (no push key, a broken store): FCM lowers the
   * priority of apps whose high-priority messages show nothing, and a message must never go
   * unnoticed, so this says "New message" without naming anything. On the phone only, as every
   * "New message" that may still get its text: a retry replaces it ([showOpenedLate]).
   */
  fun showUnreadable() {
    if (!canPost()) return
    val notification = NotificationCompat.Builder(context, NotificationChannels.MESSAGES)
      .setSmallIcon(R.drawable.ic_notification)
      .setContentTitle(context.getString(R.string.app_name))
      .setContentText(context.getString(R.string.notification_new_message))
      .setCategory(NotificationCompat.CATEGORY_MESSAGE)
      .setAutoCancel(true)
      .setLocalOnly(true)
      .setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
      .setContentIntent(launchIntent())
      .build()
    postSafely(UNREADABLE_TAG, OTHER_ID, notification)
  }

  /** The generic notification of a wake-up that could not be opened goes. */
  fun cancelUnreadable() {
    manager.cancel(UNREADABLE_TAG, OTHER_ID)
  }

  /** Adds the user's own reply to the conversation's notification (Android Auto expects this). */
  fun showOwnReply(conversationId: String, reply: CharSequence) {
    synchronized(lock) {
      val shown = shown()
      val entry = shown.conversations[conversationId] ?: return
      val line = NotificationLine(text = reply.toString(), sentAtMs = System.currentTimeMillis())
      val updated = entry.copy(lines = NotificationLines.merge(entry.lines, line))
      update(shown.copy(conversations = shown.conversations + (conversationId to updated)))
      post(updated, NotificationAlerting.SILENT)
    }
  }

  /**
   * Read on another device (a `dismiss` wake-up): removes the thread's or the conversation's lines
   * up to the read message; the notification goes when nothing is left.
   */
  fun removeRead(conversationId: String, maybeThreadId: String?, maybeUpToMessageId: String?, maybeBadge: UInt?) {
    synchronized(lock) {
      val shown = shown()
      val withBadge = maybeBadge?.let { shown.copy(maybeBadge = it.toInt()) } ?: shown
      val entry = withBadge.conversations[conversationId]
      if (entry == null) {
        update(withBadge)
        postSummary()
        return
      }
      val lines = NotificationLines.afterRead(entry.lines, maybeThreadId, maybeUpToMessageId)
      if (lines.isEmpty()) {
        update(withBadge.copy(conversations = withBadge.conversations - conversationId))
        manager.cancel(conversationId, MESSAGE_ID)
      } else {
        val updated = entry.copy(lines = lines)
        update(withBadge.copy(conversations = withBadge.conversations + (conversationId to updated)))
        if (lines != entry.lines) post(updated, NotificationAlerting.SILENT)
      }
      postSummary()
    }
  }

  /** The conversation was read here (or its notification swiped away): forget it entirely. */
  fun dismiss(conversationId: String) {
    synchronized(lock) {
      val shown = shown()
      if (conversationId !in shown.conversations) return
      update(shown.copy(conversations = shown.conversations - conversationId))
      manager.cancel(conversationId, MESSAGE_ID)
      postSummary()
    }
  }

  /**
   * Shows the current notifications again, silently, as the app lock now says (turning it on hides
   * what is already in the shade; turning it off brings the text back).
   */
  fun repostAll() {
    synchronized(lock) {
      val shown = shown()
      shown.conversations.values.forEach { entry -> post(entry, NotificationAlerting.SILENT) }
      postSummary()
    }
  }

  /** Conversations that have a notification now. */
  fun shownConversationIds(): Set<String> = synchronized(lock) { shown().conversations.keys }

  /** A ring the device's own mute/DND check kept quiet: a silent note that somebody called. */
  fun showQuietCall(name: String) {
    if (!canPost()) return
    val notification = NotificationCompat.Builder(context, NotificationChannels.OTHER)
      .setSmallIcon(R.drawable.ic_notification)
      .setContentTitle(context.getString(R.string.call_missed))
      .setContentText(name)
      .setCategory(NotificationCompat.CATEGORY_MISSED_CALL)
      .setSilent(true)
      .setAutoCancel(true)
      .setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
      .setContentIntent(launchIntent())
      .build()
    postSafely(QUIET_CALL_TAG, OTHER_ID, notification)
  }

  /**
   * A planned meeting starts soon or has started (docs/meetings.md, "Starting"): the meetings
   * channel, the start replaces the reminder, and no ringing. Join joins the meeting chat's call if
   * this device knows it as one the user is in; otherwise a tap opens the app, as for a ring.
   */
  fun showMeeting(notification: PushNotification) {
    if (!canPost()) return
    val isStart = notification.kind == PushNotificationKind.MEETING_START
    val name = notification.maybeConversationName?.takeIf { it.isNotBlank() }
    val minutes = notification.maybeMeetingMinutes?.toInt() ?: 0
    val title = when {
      isStart && name != null -> context.getString(R.string.meeting_notification_started, name)
      isStart -> context.getString(R.string.meeting_notification_started_unnamed)
      name != null -> context.getString(R.string.meeting_notification_soon, name, minutes)
      else -> context.getString(R.string.meeting_notification_soon_unnamed, minutes)
    }
    val conversationId = notification.conversationId
    val maybeMeetingId = notification.maybeMeetingId
    val maybeStartsAtMs = notification.maybeMeetingStartsAtMs
    val builder = NotificationCompat.Builder(context, NotificationChannels.MEETINGS)
      .setSmallIcon(R.drawable.ic_notification)
      .setContentTitle(title)
      .setCategory(NotificationCompat.CATEGORY_EVENT)
      .setAutoCancel(true)
      .setTimeoutAfter(if (isStart) MEETING_START_TIMEOUT_MS else minutes.coerceAtLeast(1) * 60_000L)
      .setContentIntent(openIntent(conversationId))
    notification.maybeMeetingStartsAtMs?.let { builder.setWhen(it).setShowWhen(true) }
    if (notification.isConversationVerified) {
      val join = PendingIntent.getActivity(
        context,
        conversationId.hashCode(),
        Intent(context, CallActivity::class.java)
          .setAction(CallActivity.ACTION_ANSWER)
          .putExtra(CallActivity.EXTRA_CONVERSATION_ID, conversationId)
          .apply {
            // Joining tells the server, so the user's other phones take their notice down.
            if (maybeMeetingId != null && maybeStartsAtMs != null) {
              putExtra(CallActivity.EXTRA_MEETING_ID, maybeMeetingId)
              putExtra(CallActivity.EXTRA_MEETING_STARTS_AT_MS, maybeStartsAtMs)
            }
          }
          .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
        PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
      )
      builder.addAction(R.drawable.ic_notification, context.getString(R.string.meeting_join), join)
    }
    // Swiped away (the system tells the app without a permission): the same report as Join.
    if (maybeMeetingId != null && maybeStartsAtMs != null) {
      val dismissed = NotificationActionReceiver.meetingDismissedIntent(context, conversationId, maybeMeetingId, maybeStartsAtMs)
      builder.setDeleteIntent(
        PendingIntent.getBroadcast(
          context,
          maybeMeetingId.hashCode(),
          dismissed,
          PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        ),
      )
    }
    postSafely(MEETING_TAG_PREFIX + (notification.maybeMeetingId ?: conversationId), MEETING_ID, builder.build())
  }

  /** A meeting's notice was dealt with on another device (a `dismiss` wake-up): takes it down. */
  fun removeMeeting(meetingId: String) {
    manager.cancel(MEETING_TAG_PREFIX + meetingId, MEETING_ID)
  }

  /** The diagnostics' test push arrived. */
  fun showTest(maybeSentAtMs: Long?) {
    if (!canPost()) return
    val maybeDelaySeconds = maybeSentAtMs?.let { ((System.currentTimeMillis() - it).coerceAtLeast(0) + 500) / 1000 }
    val text = maybeDelaySeconds?.let { context.getString(R.string.test_push_received_after, it) }
      ?: context.getString(R.string.test_push_received)
    val notification = NotificationCompat.Builder(context, NotificationChannels.OTHER)
      .setSmallIcon(R.drawable.ic_notification)
      .setContentTitle(context.getString(R.string.test_push_title))
      .setContentText(text)
      .setCategory(NotificationCompat.CATEGORY_STATUS)
      .setAutoCancel(true)
      .setTimeoutAfter(TEST_TIMEOUT_MS)
      .build()
    postSafely(TEST_TAG, OTHER_ID, notification)
  }

  /** Removes every notification, the shortcuts and what they remembered (sign-out). */
  fun clearAll() {
    synchronized(lock) {
      maybeShown = ShownNotifications()
      maybeLastSummary = null
      store.delete()
      manager.cancelAll()
      avatars.clear()
      runCatching { ShortcutManagerCompat.removeAllDynamicShortcuts(context) }
    }
  }

  private fun entry(
    notification: PushNotification,
    maybePrevious: ConversationNotification?,
    lines: List<NotificationLine>,
  ) = ConversationNotification(
    conversationId = notification.conversationId,
    maybeTitle = title(notification) ?: maybePrevious?.maybeTitle,
    isVerified = notification.isConversationVerified,
    isMention = notification.isMention || notification.isUrgent,
    isDirect = notification.isDirect,
    lines = lines,
  )

  private fun line(notification: PushNotification, maybePayload: ByteArray?): NotificationLine {
    val text = text(notification)
    val sentAtMs = notification.maybeMessageId?.let(NotificationLines::sentAtMs)
      ?: notification.maybeSentAtMs
      ?: System.currentTimeMillis()
    val isRetried = PushRetry.isPlaceholder(notification)
    return NotificationLine(
      maybeMessageId = notification.maybeMessageId,
      maybeThreadId = notification.maybeThreadId,
      maybeSenderKey = notification.maybeSenderId ?: text.senderName,
      senderName = text.senderName,
      text = text.text,
      sentAtMs = sentAtMs,
      // A kind this version can't show stays "New message", so it is one for bridges too.
      isPlaceholder = isRetried || notification.maybeGap == NotificationGap.NOT_SHOWABLE,
      maybePayload = maybePayload?.takeIf { isRetried }?.let { Base64.getEncoder().encodeToString(it) },
      maybeArrivedAtMs = elapsedNow().takeIf { isRetried },
    )
  }

  private fun title(notification: PushNotification): String? = text(notification).maybeConversationTitle

  private fun text(notification: PushNotification): NotificationText =
    NotificationText.from(
      maybeConversationName = notification.maybeConversationName,
      maybeSenderName = notification.maybeSenderName?.let { name ->
        // A source's name always says it's a bot.
        if (notification.isSenderSource) context.getString(R.string.source_sender, name) else name
      },
      maybeText = notification.maybeText,
      maybeFileName = notification.maybeFileName,
      maybeAttachmentKind = notification.maybeAttachmentKind,
      isDirect = notification.isDirect,
      labels = labels(),
      isAddedYou = notification.maybeGap == NotificationGap.ADDED_YOU,
    )

  private fun shown(): ShownNotifications =
    maybeShown ?: reconciled(store.read()).also { maybeShown = it }

  /**
   * Drops what the system no longer shows (swiped away while no process ran, a reboot), so it
   * doesn't come back with the next message.
   */
  private fun reconciled(saved: ShownNotifications): ShownNotifications {
    val system = context.getSystemService(NotificationManager::class.java) ?: return saved
    val active = runCatching { system.activeNotifications.filter { it.id == MESSAGE_ID }.mapNotNull { it.tag }.toSet() }
      .getOrNull() ?: return saved
    val kept = saved.conversations.filterKeys { it in active }
    return if (kept.size == saved.conversations.size) saved else saved.copy(conversations = kept).also(store::write)
  }

  private fun update(shown: ShownNotifications) {
    maybeShown = shown
    store.write(shown)
  }

  private fun post(entry: ConversationNotification, alerting: NotificationAlerting) {
    if (!canPost()) return
    val privacy = MessagePrivacy.of(contentPrefs.content.value, isAppLockOn(), entry.isVerified)
    val spec = spec(entry, privacy, alerting) ?: return
    val revision = revisions.incrementAndGet()
    postSafely(entry.conversationId, MESSAGE_ID, build(spec, revision))
    confirmLater(entry.conversationId, revision, alerting)
    spec.maybeShortcut?.let(::pushShortcut)
    if (privacy.showsNames) fetchAvatars(entry)
  }

  private fun spec(entry: ConversationNotification, privacy: MessagePrivacy, alerting: NotificationAlerting) =
    MessageNotificationSpecs.conversation(entry, privacy, alerting == NotificationAlerting.ALERT, labels(), canBubble(entry))
      ?.let { if (alerting == NotificationAlerting.REFRESH) it.copy(alerting = alerting) else it }

  /** Opted in for this conversation, and the system allows bubbles (the privacy rules apply in the spec). */
  private fun canBubble(entry: ConversationNotification): Boolean =
    entry.isDirect && bubbles.isEnabled(entry.conversationId) && bubbles.isAllowedBySystem()

  /**
   * Android drops updates of an app that posts more than about 5 notifications a second, without
   * saying so: in a burst (each message posts its conversation, the summary, maybe a picture) the
   * shade could stay on "New message", or an alert go missing, while this class counts it as done.
   * So once a conversation's posts have stopped for a moment, the system's copy is looked at, and
   * what didn't get through is posted once more ([PostConfirm]). Never again after that: no loop.
   * Caller holds [lock].
   */
  private fun confirmLater(conversationId: String, revision: Long, alerting: NotificationAlerting) {
    confirms[conversationId] = (confirms[conversationId] ?: PostConfirm()).after(revision, alerting)
    confirmJobs.remove(conversationId)?.cancel()
    confirmJobs[conversationId] = scope.launch {
      delay(POST_CONFIRM_MS)
      synchronized(lock) { confirm(conversationId) }
    }
  }

  private fun confirm(conversationId: String) {
    confirmJobs.remove(conversationId)
    val pending = confirms.remove(conversationId) ?: return
    // Gone meanwhile, read or swiped away: nothing to bring back.
    val entry = shown().conversations[conversationId] ?: return
    val activeRevision = activeRevision(conversationId) ?: return
    val alerting = pending.repostAs(activeRevision) ?: return
    val privacy = MessagePrivacy.of(contentPrefs.content.value, isAppLockOn(), entry.isVerified)
    val spec = spec(entry, privacy, alerting) ?: return
    Log.i(TAG, "a notification update did not get through; posted again: $alerting")
    postSafely(conversationId, MESSAGE_ID, build(spec, revisions.incrementAndGet()))
  }

  private fun activeRevision(conversationId: String): Long? {
    val system = context.getSystemService(NotificationManager::class.java) ?: return null
    val active = runCatching { system.activeNotifications }.getOrNull() ?: return null
    return active.firstOrNull { it.id == MESSAGE_ID && it.tag == conversationId }
      ?.notification?.extras?.getLong(EXTRA_REVISION, 0)
  }

  private fun build(spec: MessageNotificationSpec, revision: Long): Notification {
    val isAlerting = spec.alerting == NotificationAlerting.ALERT
    val builder = NotificationCompat.Builder(context, spec.channel)
      .setSmallIcon(R.drawable.ic_notification)
      .setCategory(NotificationCompat.CATEGORY_MESSAGE)
      // Also with MessagingStyle: watch bridges that read only title and text show these.
      .setContentTitle(spec.title)
      .setContentText(spec.text)
      .setWhen(spec.whenMs)
      .setShowWhen(true)
      .setAutoCancel(true)
      .setOnlyAlertOnce(spec.alerting == NotificationAlerting.SILENT)
      .setSilent(!isAlerting)
      // Bridged to watches (Wear OS, vendor apps) and Android Auto once it has the text: never
      // "New message".
      .setLocalOnly(spec.isLocalOnly)
      .setGroup(GROUP_MESSAGES)
      .setGroupAlertBehavior(NotificationCompat.GROUP_ALERT_CHILDREN)
      .addExtras(Bundle().apply { putLong(EXTRA_REVISION, revision) })
      .setContentIntent(openIntent(spec.conversationId))
      .setDeleteIntent(actionIntent(NotificationActionReceiver.ACTION_DISMISSED, spec.conversationId, isMutable = false))
      // The lock screen names the sender but not the text unless the user unlocks.
      .setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
      .setPublicVersion(publicVersion(spec.channel, spec.publicTitle, spec.publicText))
    if (spec.messageCount > 1) builder.setNumber(spec.messageCount)
    spec.maybeConversation?.let { conversation -> builder.setStyle(messagingStyle(conversation)) }
    spec.maybeLargeIconSender?.let { sender ->
      builder.setLargeIcon(avatars.maybeBitmap(sender.key) ?: PersonIcons.bitmap(sender.key, sender.name))
    }
    spec.maybeShortcut?.let { shortcut -> builder.setShortcutId(shortcut.id).setLocusId(LocusIdCompat(shortcut.id)) }
    if (spec.isBubble) spec.maybeShortcut?.let { shortcut -> builder.setBubbleMetadata(bubbleMetadata(shortcut)) }
    spec.actions.forEach { action ->
      builder.addAction(
        when (action) {
          MessageAction.REPLY -> replyAction(spec.conversationId, spec.maybeReplyThreadId)
          MessageAction.MARK_READ -> markReadAction(spec.conversationId)
          MessageAction.MUTE -> muteAction(spec.conversationId)
        },
      )
    }
    return builder.build()
  }

  /**
   * The conversation as a bubble: [BubbleActivity] shows it. Bubbles need a mutable PendingIntent
   * (the system adds the display bounds), so the intent names its component and carries the launch
   * token like the notification's own tap.
   */
  private fun bubbleMetadata(shortcut: ShortcutSpec): NotificationCompat.BubbleMetadata {
    val intent = Intent(context, BubbleActivity::class.java)
      .setData(conversationUri(shortcut.id))
      .putExtra(MainActivity.EXTRA_CONVERSATION_ID, shortcut.id)
      .putExtra(MainActivity.EXTRA_LAUNCH_TOKEN, LaunchToken.of(context))
    val pending = PendingIntent.getActivity(context, 0, intent, PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_MUTABLE)
    val label = shortcut.label.ifBlank { context.getString(R.string.app_name) }
    val key = shortcut.maybePerson?.key ?: shortcut.id
    return NotificationCompat.BubbleMetadata.Builder(pending, IconCompat.createWithAdaptiveBitmap(PersonIcons.bitmap(key, label)))
      .setDesiredHeight(BUBBLE_HEIGHT_DP)
      .setAutoExpandBubble(false)
      .setSuppressNotification(false)
      .build()
  }

  private fun messagingStyle(conversation: ConversationStyleSpec): NotificationCompat.MessagingStyle {
    val me = Person.Builder().setName(context.getString(R.string.you)).setKey(SELF_KEY).build()
    val style = NotificationCompat.MessagingStyle(me)
      .setConversationTitle(conversation.maybeConversationTitle)
      .setGroupConversation(conversation.isGroupConversation)
    conversation.messages.forEach { message ->
      style.addMessage(highlighted(message.text), message.sentAtMs, message.maybeSender?.let(::person))
    }
    return style
  }

  /**
   * Fetches the senders' pictures in the background (never from the push); a picture that arrives
   * replaces the initials in the notification already shown, silently.
   */
  private fun fetchAvatars(entry: ConversationNotification) {
    val senders = entry.lines.mapNotNull { it.maybeSenderKey }.distinct()
    senders.forEach { sender ->
      scope.launch {
        if (!avatars.refresh(sender)) return@launch
        synchronized(lock) {
          shown().conversations[entry.conversationId]?.let { current -> post(current, NotificationAlerting.SILENT) }
        }
      }
    }
  }

  private fun person(sender: NotificationSender): Person =
    Person.Builder()
      .setName(sender.name)
      .setKey(sender.key)
      .setIcon(avatars.maybeIcon(sender.key) ?: PersonIcons.of(sender.key, sender.name))
      .build()

  /** `@mentions` in bold, as in the app. */
  private fun highlighted(text: String): CharSequence {
    val ranges = NotificationStyling.mentionRanges(text)
    if (ranges.isEmpty()) return text
    return SpannableString(text).apply {
      ranges.forEach { setSpan(StyleSpan(Typeface.BOLD), it.first, it.last + 1, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE) }
    }
  }

  private fun labels() = NotificationText.Labels(
    appName = context.getString(R.string.app_name),
    newMessage = context.getString(R.string.notification_new_message),
    addedYou = context.getString(R.string.notification_added_you),
    newMessageFrom = { context.getString(R.string.notification_new_message_from, it) },
    newMessagesFrom = { count, names ->
      context.resources.getQuantityString(R.plurals.notification_new_messages_from, count, count, names)
    },
    newMessages = { count -> context.resources.getQuantityString(R.plurals.notification_new_messages, count, count) },
    attachment = { kind, fileName ->
      when (kind) {
        NotificationAttachmentKind.IMAGE -> context.getString(R.string.notification_photo)
        NotificationAttachmentKind.VIDEO -> context.getString(R.string.notification_video)
        NotificationAttachmentKind.AUDIO -> context.getString(R.string.notification_audio)
        NotificationAttachmentKind.FILE -> context.getString(R.string.notification_file, fileName)
      }
    },
  )

  /**
   * The summary that groups the conversations once two or more show
   * ([MessageNotificationSpecs.summary]); never alerts, carries the badge count, and is posted
   * again only when it changed, so watch bridges that mirror summaries don't repeat it.
   */
  private fun postSummary() {
    val shown = shown()
    if (shown.conversations.isEmpty()) {
      maybeLastSummary = null
      manager.cancel(null, SUMMARY_ID)
      return
    }
    if (!canPost()) return
    // Names only (none when names are hidden): the summary may show where the text does not.
    val privacy = MessagePrivacy.of(contentPrefs.content.value, isAppLockOn(), isVerified = false)
    val isShown = isSummaryShown()
    val spec = MessageNotificationSpecs.summary(
      conversations = shown.conversations.values,
      privacy = privacy,
      maybeBadge = shown.maybeBadge,
      isShown = isShown,
      labels = labels(),
      conversationsTitle = { count -> context.resources.getQuantityString(R.plurals.notification_summary, count, count) },
    ) ?: return
    if (isShown && spec == maybeLastSummary) return
    val inbox = NotificationCompat.InboxStyle().setBigContentTitle(spec.title)
    spec.lines.forEach(inbox::addLine)
    val summary = NotificationCompat.Builder(context, NotificationChannels.MESSAGES)
      .setSmallIcon(R.drawable.ic_notification)
      .setContentTitle(spec.title)
      .setContentText(spec.text)
      .setStyle(inbox)
      .setCategory(NotificationCompat.CATEGORY_MESSAGE)
      .setGroup(GROUP_MESSAGES)
      .setGroupSummary(true)
      .setGroupAlertBehavior(NotificationCompat.GROUP_ALERT_CHILDREN)
      .setSilent(true)
      .setOnlyAlertOnce(true)
      .setLocalOnly(false)
      .setAutoCancel(true)
      .setNumber(spec.number)
      .setBadgeIconType(NotificationCompat.BADGE_ICON_SMALL)
      .setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
      .setPublicVersion(publicVersion(NotificationChannels.MESSAGES, spec.publicTitle, spec.publicText))
      .setContentIntent(launchIntent())
      .build()
    postSafely(null, SUMMARY_ID, summary)
    maybeLastSummary = spec
  }

  /** Whether the system shows the summary now (the process may have restarted since it was posted). */
  private fun isSummaryShown(): Boolean {
    val system = context.getSystemService(NotificationManager::class.java) ?: return maybeLastSummary != null
    return runCatching { system.activeNotifications.any { it.id == SUMMARY_ID && it.tag == null } }
      .getOrElse { maybeLastSummary != null }
  }

  /** A long-lived shortcut per conversation: what makes Android treat the notification as one. */
  private fun pushShortcut(spec: ShortcutSpec) {
    val label = spec.label.ifBlank { context.getString(R.string.app_name) }
    val shortcut = ShortcutInfoCompat.Builder(context, spec.id)
      .setShortLabel(label)
      .setLongLived(true)
      .setLocusId(LocusIdCompat(spec.id))
      // Also a direct share target, so this push doesn't drop the share sheet's entry (#829).
      .setCategories(setOf(ShareShortcuts.CATEGORY))
      .setIcon(shortcutIcon(spec, label))
      .setIntent(openConversationIntent(spec.id).setAction(Intent.ACTION_VIEW))
      .apply { spec.maybePerson?.let { setPerson(person(it)) } }
      .build()
    try {
      ShortcutManagerCompat.pushDynamicShortcut(context, shortcut)
    } catch (failure: Exception) {
      // Rate-limited or refused by the launcher: the notification still shows, just not as a conversation.
      Log.w(TAG, "conversation shortcut not pushed: ${failure.javaClass.simpleName}")
    }
  }

  /** A person's picture for a one-to-one chat, else the generated avatar of the conversation. */
  private fun shortcutIcon(spec: ShortcutSpec, label: String): IconCompat {
    val maybeSender = spec.maybePerson
    return maybeSender?.let { avatars.maybeIcon(it.key) } ?: PersonIcons.of(maybeSender?.key ?: spec.id, label)
  }

  private fun postSafely(maybeTag: String?, id: Int, notification: Notification) {
    try {
      manager.notify(maybeTag, id, notification)
    } catch (denied: SecurityException) {
      // The permission was revoked between the check and the post.
      Log.w(TAG, "notification not posted: permission missing")
    }
  }

  private fun isAppLockOn(): Boolean = AppLockPrefs.prefs(context).isEnabled

  /**
   * What the lock screen shows while it hides sensitive content: who wrote, never what (no names
   * when the setting hides them). Watch bridges that follow the lock screen's privacy show this
   * too while the phone is locked.
   */
  private fun publicVersion(channel: String, title: String, text: String) = NotificationCompat.Builder(context, channel)
    .setSmallIcon(R.drawable.ic_notification)
    .setContentTitle(title)
    .setContentText(text)
    .setCategory(NotificationCompat.CATEGORY_MESSAGE)
    .build()

  /** Replies where the newest message is: in its thread if it was a thread reply. */
  private fun replyAction(conversationId: String, maybeThreadId: String?): NotificationCompat.Action {
    val input = RemoteInput.Builder(NotificationActionReceiver.KEY_REPLY)
      .setLabel(context.getString(R.string.notification_reply))
      .build()
    // Mutable: the system adds the typed or spoken text to this intent.
    val pending = actionIntent(NotificationActionReceiver.ACTION_REPLY, conversationId, isMutable = true, maybeThreadId)
    return NotificationCompat.Action.Builder(R.drawable.ic_notification, context.getString(R.string.notification_reply), pending)
      .addRemoteInput(input)
      .setSemanticAction(NotificationCompat.Action.SEMANTIC_ACTION_REPLY)
      .setShowsUserInterface(false)
      .setAllowGeneratedReplies(true)
      // Not from the lock screen without unlocking (Android 12+; Android Auto is unaffected).
      .setAuthenticationRequired(true)
      .build()
  }

  private fun markReadAction(conversationId: String): NotificationCompat.Action {
    val pending = actionIntent(NotificationActionReceiver.ACTION_MARK_READ, conversationId, isMutable = false)
    return NotificationCompat.Action.Builder(R.drawable.ic_notification, context.getString(R.string.notification_mark_read), pending)
      .setSemanticAction(NotificationCompat.Action.SEMANTIC_ACTION_MARK_AS_READ)
      .setShowsUserInterface(false)
      .setAuthenticationRequired(true)
      .build()
  }

  private fun muteAction(conversationId: String): NotificationCompat.Action {
    val pending = actionIntent(NotificationActionReceiver.ACTION_MUTE, conversationId, isMutable = false)
    return NotificationCompat.Action.Builder(R.drawable.ic_notification, context.getString(R.string.notification_mute), pending)
      .setSemanticAction(NotificationCompat.Action.SEMANTIC_ACTION_MUTE)
      .setShowsUserInterface(false)
      .setAuthenticationRequired(true)
      .build()
  }

  /** Each conversation's intents differ in their data URI, so their PendingIntents never collide. */
  private fun actionIntent(
    action: String,
    conversationId: String,
    isMutable: Boolean,
    maybeThreadId: String? = null,
  ): PendingIntent {
    val intent = NotificationActionReceiver.intent(context, action, conversationId, maybeThreadId)
      .setData(conversationUri(conversationId))
    val mutability = if (isMutable) PendingIntent.FLAG_MUTABLE else PendingIntent.FLAG_IMMUTABLE
    return PendingIntent.getBroadcast(context, 0, intent, PendingIntent.FLAG_UPDATE_CURRENT or mutability)
  }

  private fun openIntent(conversationId: String): PendingIntent {
    val intent = openConversationIntent(conversationId)
      .setData(conversationUri(conversationId))
      .addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP)
    return PendingIntent.getActivity(context, 0, intent, PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE)
  }

  private fun openConversationIntent(conversationId: String): Intent =
    Intent(context, MainActivity::class.java)
      .putExtra(MainActivity.EXTRA_CONVERSATION_ID, conversationId)
      .putExtra(MainActivity.EXTRA_LAUNCH_TOKEN, LaunchToken.of(context))

  private fun launchIntent(): PendingIntent =
    PendingIntent.getActivity(
      context,
      0,
      Intent(context, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP),
      PendingIntent.FLAG_IMMUTABLE,
    )

  private fun conversationUri(conversationId: String): Uri = Uri.fromParts(URI_SCHEME, conversationId, null)

  private fun canPost(): Boolean =
    Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU ||
      ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED

  companion object {
    /** Message notifications are told apart by their tag (the conversation ID); the ID is fixed. */
    const val MESSAGE_ID = 1
    private const val SUMMARY_ID = 2
    private const val OTHER_ID = 3
    private const val MEETING_ID = 5
    private const val MEETING_TAG_PREFIX = "meeting:"
    /** The notice of a started meeting goes away by itself if nobody reacted to it. */
    private const val MEETING_START_TIMEOUT_MS = 60 * 60_000L
    private const val TEST_TAG = "test"
    private const val QUIET_CALL_TAG = "quiet_call"
    private const val UNREADABLE_TAG = "unreadable"
    private const val QUIET_NOTE_TAG = "quiet_note"
    private const val QUIET_NOTE_MS = 3_000L
    private const val POST_CONFIRM_MS = 1_500L
    private const val BUBBLE_HEIGHT_DP = 600
    private const val EXTRA_REVISION = "de.zeitgleich.teamcraft.REVISION"
    private const val GROUP_MESSAGES = "de.zeitgleich.teamcraft.MESSAGES"
    private const val SELF_KEY = "self"
    private const val URI_SCHEME = "teamcraft-conversation"
    private const val TEST_TIMEOUT_MS = 5L * 60 * 1000
    private const val TAG = "TeamCraftNotify"
  }
}

/** A placeholder whose alert is held: its sealed wake-up, and when it came (`elapsedRealtime`). */
class HeldLine(val payload: ByteArray, val maybeArrivedAtMs: Long?)

package de.zeitgleich.teamcraft.widget

import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.glance.ColorFilter
import androidx.glance.GlanceId
import androidx.glance.GlanceModifier
import androidx.glance.GlanceTheme
import androidx.glance.Image
import androidx.glance.ImageProvider
import androidx.glance.LocalContext
import androidx.glance.action.Action
import androidx.glance.action.clickable
import androidx.glance.appwidget.GlanceAppWidget
import androidx.glance.appwidget.GlanceAppWidgetReceiver
import androidx.glance.appwidget.SizeMode
import androidx.glance.appwidget.action.actionStartActivity
import androidx.glance.appwidget.appWidgetBackground
import androidx.glance.appwidget.cornerRadius
import androidx.glance.appwidget.lazy.LazyColumn
import androidx.glance.appwidget.lazy.items
import androidx.glance.appwidget.provideContent
import androidx.glance.background
import androidx.glance.color.ColorProvider
import androidx.glance.layout.Alignment
import androidx.glance.layout.Box
import androidx.glance.layout.Column
import androidx.glance.layout.Row
import androidx.glance.layout.Spacer
import androidx.glance.layout.fillMaxSize
import androidx.glance.layout.fillMaxWidth
import androidx.glance.layout.height
import androidx.glance.layout.padding
import androidx.glance.layout.size
import androidx.glance.layout.width
import androidx.glance.semantics.contentDescription
import androidx.glance.semantics.semantics
import androidx.glance.text.FontWeight
import androidx.glance.text.Text
import androidx.glance.text.TextAlign
import androidx.glance.text.TextStyle
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.TeamCraftApplication
import de.zeitgleich.teamcraft.notifications.openConversationIntent
import de.zeitgleich.teamcraft.ui.LaunchShortcut
import de.zeitgleich.teamcraft.ui.MainActivity

/** The brand's badge red (Theme.kt, `badge`): white numbers on it reach 4.5:1, light and dark. */
private val BADGE = Color(0xFFC8441F)

/** Tells the widget's own intents apart, so their PendingIntents never replace each other. */
private const val WIDGET_URI_SCHEME = "teamcraft-widget"

/**
 * "Unread" on the home screen (issue #2076): the conversations waiting for the user and a button
 * for a new message. A tap opens a conversation the way its notification does. What it shows is
 * the sealed [WidgetSnapshot], already cut to the privacy settings; it reads nothing else.
 */
class UnreadWidget : GlanceAppWidget() {
  override val sizeMode: SizeMode = SizeMode.Exact

  override suspend fun provideGlance(context: Context, id: GlanceId) {
    val widgets = TeamCraftApplication.graph(context).homeWidgets
    val initial = widgets.currentSnapshot()
    provideContent {
      // Follows each change while the session lives; a new session starts from `initial`.
      val maybeSnapshot by widgets.snapshot.collectAsState(initial)
      GlanceTheme { WidgetContent(maybeSnapshot) }
    }
  }
}

/** Placed, updated by the launcher, removed: the system's side of [UnreadWidget]. */
class UnreadWidgetReceiver : GlanceAppWidgetReceiver() {
  override val glanceAppWidget: GlanceAppWidget = UnreadWidget()

  override fun onUpdate(context: Context, appWidgetManager: android.appwidget.AppWidgetManager, appWidgetIds: IntArray) {
    super.onUpdate(context, appWidgetManager, appWidgetIds)
    // Just placed, or the first since an update of the app: read once. Later updates come from
    // the core's changes (HomeWidgets).
    val widgets = TeamCraftApplication.graph(context).homeWidgets
    if (!widgets.hasSnapshot()) widgets.requestRefresh()
  }
}

@Composable
private fun WidgetContent(maybeSnapshot: WidgetSnapshot?) {
  val context = LocalContext.current
  Column(
    modifier = GlanceModifier
      .fillMaxSize()
      .appWidgetBackground()
      .cornerRadius(24.dp)
      .background(GlanceTheme.colors.widgetBackground)
      .padding(horizontal = 12.dp, vertical = 8.dp),
  ) {
    Header(maybeSnapshot)
    val snapshot = maybeSnapshot
    when {
      snapshot == null -> Message(context.getString(R.string.widget_signed_out), maybeIcon = null)
      snapshot.privacy == WidgetPrivacy.LOCKED -> Message(context.getString(R.string.widget_locked), maybeIcon = R.drawable.ic_lock)
      snapshot.unreadConversations == 0 -> Message(context.getString(R.string.widget_caught_up), maybeIcon = null)
      snapshot.privacy == WidgetPrivacy.COUNT_ONLY -> Message(
        context.resources.getQuantityString(R.plurals.widget_unread_conversations, snapshot.unreadConversations, snapshot.unreadConversations),
        maybeIcon = null,
      )
      else -> Rows(snapshot)
    }
  }
}

/** The title, which opens the app, and the new-message button. */
@Composable
private fun Header(maybeSnapshot: WidgetSnapshot?) {
  val context = LocalContext.current
  Row(
    modifier = GlanceModifier.fillMaxWidth().padding(start = 4.dp, bottom = 4.dp),
    verticalAlignment = Alignment.CenterVertically,
  ) {
    Text(
      text = context.getString(R.string.widget_title),
      style = TextStyle(color = GlanceTheme.colors.onSurface, fontSize = 17.sp, fontWeight = FontWeight.Medium),
      maxLines = 1,
      modifier = GlanceModifier.defaultWeight().clickable(openApp(context)),
    )
    // Signed out there is nobody to write to.
    if (maybeSnapshot != null) ComposeButton()
  }
}

@Composable
private fun ComposeButton() {
  val context = LocalContext.current
  val label = context.getString(R.string.shortcut_new_message)
  // A 48 dp target around a 40 dp circle.
  Box(
    modifier = GlanceModifier
      .size(48.dp)
      .clickable(newMessage(context))
      .semantics { contentDescription = label },
    contentAlignment = Alignment.Center,
  ) {
    Box(
      modifier = GlanceModifier.size(40.dp).cornerRadius(20.dp).background(GlanceTheme.colors.primary),
      contentAlignment = Alignment.Center,
    ) {
      Image(
        provider = ImageProvider(R.drawable.ic_shortcut_new_message),
        contentDescription = null,
        colorFilter = ColorFilter.tint(GlanceTheme.colors.onPrimary),
        modifier = GlanceModifier.size(20.dp),
      )
    }
  }
}

@Composable
private fun Rows(snapshot: WidgetSnapshot) {
  LazyColumn(modifier = GlanceModifier.fillMaxSize()) {
    items(snapshot.rows, itemId = { it.conversationId.toLongOrNull() ?: it.conversationId.hashCode().toLong() }) { row ->
      ConversationRow(row)
    }
  }
}

@Composable
private fun ConversationRow(row: WidgetRow) {
  val context = LocalContext.current
  Row(
    modifier = GlanceModifier
      .fillMaxWidth()
      .height(if (row.maybePreview == null) 48.dp else 56.dp)
      .cornerRadius(12.dp)
      .clickable(actionStartActivity(openConversationIntent(context, row.conversationId)))
      .padding(horizontal = 4.dp),
    verticalAlignment = Alignment.CenterVertically,
  ) {
    Column(modifier = GlanceModifier.defaultWeight()) {
      Text(
        text = row.name,
        style = TextStyle(color = GlanceTheme.colors.onSurface, fontSize = 15.sp, fontWeight = FontWeight.Medium),
        maxLines = 1,
      )
      row.maybePreview?.let { preview ->
        Text(
          text = preview,
          style = TextStyle(color = GlanceTheme.colors.onSurfaceVariant, fontSize = 13.sp),
          maxLines = 1,
        )
      }
    }
    if (row.countLabel.isNotEmpty()) {
      Spacer(GlanceModifier.width(8.dp))
      CountPill(row.countLabel)
    }
  }
}

@Composable
private fun CountPill(label: String) {
  val context = LocalContext.current
  Box(
    modifier = GlanceModifier
      .height(22.dp)
      .cornerRadius(11.dp)
      .background(ColorProvider(day = BADGE, night = BADGE))
      .padding(horizontal = 7.dp)
      .semantics { contentDescription = context.getString(R.string.widget_unread_count, label) },
    contentAlignment = Alignment.Center,
  ) {
    Text(
      text = label,
      style = TextStyle(color = ColorProvider(day = Color.White, night = Color.White), fontSize = 12.sp, fontWeight = FontWeight.Bold),
      maxLines = 1,
    )
  }
}

/** A calm line in the middle of the widget: empty, locked, signed out or names hidden. */
@Composable
private fun Message(text: String, maybeIcon: Int?) {
  val context = LocalContext.current
  Column(
    modifier = GlanceModifier.fillMaxSize().clickable(openApp(context)).padding(horizontal = 8.dp),
    verticalAlignment = Alignment.CenterVertically,
    horizontalAlignment = Alignment.CenterHorizontally,
  ) {
    maybeIcon?.let { icon ->
      Image(
        provider = ImageProvider(icon),
        contentDescription = null,
        colorFilter = ColorFilter.tint(GlanceTheme.colors.onSurfaceVariant),
        modifier = GlanceModifier.size(20.dp),
      )
      Spacer(GlanceModifier.height(6.dp))
    }
    Text(
      text = text,
      style = TextStyle(color = GlanceTheme.colors.onSurfaceVariant, fontSize = 14.sp, textAlign = TextAlign.Center),
      maxLines = 3,
    )
  }
}

private fun openApp(context: Context): Action =
  actionStartActivity(
    Intent(context, MainActivity::class.java)
      .setAction(Intent.ACTION_MAIN)
      .setData(Uri.fromParts(WIDGET_URI_SCHEME, "open", null))
      .addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP),
  )

/** The launcher's "New message" shortcut (#2061): the same entry path. */
private fun newMessage(context: Context): Action =
  actionStartActivity(
    Intent(context, MainActivity::class.java)
      .setAction(Intent.ACTION_MAIN)
      .putExtra(LaunchShortcut.EXTRA, LaunchShortcut.NewMessage.id)
      .setData(Uri.fromParts(WIDGET_URI_SCHEME, LaunchShortcut.NewMessage.id, null))
      .addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP),
  )

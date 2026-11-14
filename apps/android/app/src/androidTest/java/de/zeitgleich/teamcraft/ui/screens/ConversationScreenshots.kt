package de.zeitgleich.teamcraft.ui.screens

import android.content.pm.ActivityInfo
import android.content.res.Configuration
import android.graphics.Bitmap
import androidx.activity.compose.setContent
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.Density
import androidx.lifecycle.ViewModelProvider
import androidx.test.core.app.ActivityScenario
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import org.junit.Assume.assumeTrue
import org.junit.Test
import org.junit.runner.RunWith
import de.zeitgleich.teamcraft.core.ChatSnapshot
import de.zeitgleich.teamcraft.core.Conversation
import de.zeitgleich.teamcraft.core.FileInfo
import de.zeitgleich.teamcraft.core.FileTransfer
import de.zeitgleich.teamcraft.core.ImageBytes
import de.zeitgleich.teamcraft.core.Message
import de.zeitgleich.teamcraft.core.MessageExtras
import de.zeitgleich.teamcraft.core.Reaction
import de.zeitgleich.teamcraft.core.User
import de.zeitgleich.teamcraft.ui.FirstUnread
import de.zeitgleich.teamcraft.ui.MainActivity
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.MessageLayout
import de.zeitgleich.teamcraft.ui.MessageLayoutPrefs
import de.zeitgleich.teamcraft.ui.theme.TeamCraftTheme

private const val MINUTE_MILLIS = 60_000L
/** Long side of the sample thumbnails, as senders make them since #315. */
private const val THUMBNAIL_PX = 640
private const val CONVERSATION = "c1"
/** The oldest sample message: the scrolled pictures show it at the top. */
private const val FIRST_MESSAGE = "1"
/** The largest text size Android's settings offer. */
private const val LARGE_FONT_SCALE = 2f

/**
 * Screenshots of a conversation (#315, #309) in light and dark: image previews, the composer, the
 * attach sheet and landscape. Placeholder data only. Like [MainScreenScreenshots]
 * it runs only when asked for (`-e screenshots true`) and writes PNGs to the app's external files
 * directory (`screenshots/`).
 */
@RunWith(AndroidJUnit4::class)
class ConversationScreenshots {
  @Test
  fun conversationInLightAndDark() {
    assumeTrue(InstrumentationRegistry.getArguments().getString("screenshots") == "true")
    for (isDark in listOf(false, true)) {
      val theme = if (isDark) "dark" else "light"
      shoot("conversation-$theme", isDark) { viewModel -> ConversationSample(viewModel) }
      shoot("attach-$theme", isDark) { viewModel ->
        ConversationSample(viewModel)
        AttachSheet(rememberAttachmentPickers(viewModel, onMediaFlow = {}) {}, onDismiss = {})
      }
    }
    shoot("conversation-landscape-light", isDark = false, isLandscape = true) { viewModel -> ConversationSample(viewModel) }
    // The floating composer (#1966): with the reply bar above the field, and at the largest text.
    try {
      ReplyTarget.set(CONVERSATION, messages().first(), "Sam Doe")
      for (isDark in listOf(false, true)) {
        shoot("conversation-reply-${if (isDark) "dark" else "light"}", isDark) { viewModel -> ConversationSample(viewModel) }
      }
    } finally {
      ReplyTarget.current.value = null
    }
    shoot("conversation-large-text-light", isDark = false, fontScale = LARGE_FONT_SCALE) { viewModel -> ConversationSample(viewModel) }
    // Scrolled back, so newer messages run on behind the composer and the navigation bar under one
    // scrim, with no solid band at the bottom edge.
    for (isDark in listOf(false, true)) {
      shoot("conversation-scrolled-${if (isDark) "dark" else "light"}", isDark) { viewModel ->
        ConversationSample(viewModel, maybeFocusMessage = FIRST_MESSAGE)
      }
    }
    shoot("conversation-scrolled-landscape-light", isDark = false, isLandscape = true) { viewModel ->
      ConversationSample(viewModel, maybeFocusMessage = FIRST_MESSAGE)
    }
    // The desktop's comfortable and compact lists instead of bubbles (#598, #1488).
    val layout = MessageLayoutPrefs.of(InstrumentationRegistry.getInstrumentation().targetContext)
    try {
      for (choice in listOf(MessageLayout.Comfortable, MessageLayout.Compact)) {
        layout.setLayout(choice)
        for (isDark in listOf(false, true)) {
          shoot("conversation-${choice.slug}-${if (isDark) "dark" else "light"}", isDark) { viewModel -> ConversationSample(viewModel) }
        }
      }
    } finally {
      layout.setLayout(MessageLayout.Bubbles)
    }
  }

  private fun shoot(
    name: String,
    isDark: Boolean,
    isLandscape: Boolean = false,
    fontScale: Float = 1f,
    content: @Composable (MainViewModel) -> Unit,
  ) {
    val instrumentation = InstrumentationRegistry.getInstrumentation()
    ActivityScenario.launch(MainActivity::class.java).use { scenario ->
      scenario.onActivity { activity ->
        // Explicit both ways: on a device the rotation of the previous picture would carry over.
        activity.requestedOrientation =
          if (isLandscape) ActivityInfo.SCREEN_ORIENTATION_LANDSCAPE else ActivityInfo.SCREEN_ORIENTATION_PORTRAIT
      }
      instrumentation.waitForIdleSync()
      scenario.onActivity { activity ->
        val viewModel = ViewModelProvider(activity)[MainViewModel::class.java]
        activity.setContent {
          val configuration = Configuration(LocalConfiguration.current).apply {
            uiMode = (uiMode and Configuration.UI_MODE_NIGHT_MASK.inv()) or
              (if (isDark) Configuration.UI_MODE_NIGHT_YES else Configuration.UI_MODE_NIGHT_NO)
          }
          val density = LocalDensity.current
          CompositionLocalProvider(
            LocalConfiguration provides configuration,
            LocalDensity provides Density(density.density, density.fontScale * fontScale),
          ) {
            TeamCraftTheme { content(viewModel) }
          }
        }
      }
      instrumentation.waitForIdleSync()
      // Not timing a result: lets decoding, the sheet's animation and fonts settle before the picture.
      Thread.sleep(4_000)
      val bitmap = instrumentation.uiAutomation.takeScreenshot()
      val directory = File(instrumentation.targetContext.getExternalFilesDir(null), "screenshots").apply { mkdirs() }
      File(directory, "$name.png").outputStream().use { bitmap.compress(Bitmap.CompressFormat.PNG, 100, it) }
    }
  }

  /**
   * The conversation screen's parts with sample data (the screen itself reads the core), in the
   * screen's own scaffold: the list runs on behind the floating composer. With [maybeFocusMessage]
   * the list opens with that message at the top instead of at the end.
   */
  @Composable
  private fun ConversationSample(viewModel: MainViewModel, maybeFocusMessage: String? = null) {
    ChatScaffold(
      viewModel = viewModel,
      topBar = { ChromeTopBar(title = "Sam Doe", maybeOnBack = {}) },
      bottomBar = { Composer(viewModel, CONVERSATION, maybeThread = null, snapshot = SNAPSHOT) },
    ) { listBottomInset ->
      MessageFeed(
        viewModel = viewModel,
        snapshot = SNAPSHOT,
        messages = if (maybeFocusMessage == null) messages() else messages() + followUps(),
        extras = extras(),
        firstUnread = FirstUnread.None,
        sentCount = 0,
        onOpenThread = {},
        maybeOnReply = {},
        onLongPress = {},
        modifier = Modifier.weight(1f),
        bottomInset = listBottomInset,
        maybeFocusMessage = maybeFocusMessage,
      )
    }
  }

  private fun messages(): List<Message> {
    val start = System.currentTimeMillis() - 30 * MINUTE_MILLIS
    fun message(id: String, author: String, minute: Int, body: String, reactions: List<Reaction> = emptyList()) =
      Message(id = id, conversation = CONVERSATION, author = author, sentAt = start + minute * MINUTE_MILLIS, body = body, reactions = reactions)
    return listOf(
      message(FIRST_MESSAGE, SAM, 0, "Here are the photos from the site visit"),
      message("2", SAM, 1, ""),
      message("3", SAM, 2, ""),
      message("4", ME, 6, "Great, thanks! The second one is perfect for the report", listOf(Reaction("👍", 1, false))),
      message("5", SAM, 9, ""),
      message("6", ME, 12, "Sending the draft in a minute"),
    )
  }

  /** More of the conversation, so the scrolled pictures have messages behind the composer. */
  private fun followUps(): List<Message> {
    val start = System.currentTimeMillis() - 15 * MINUTE_MILLIS
    val lines = listOf(
      SAM to "Perfect, take your time",
      ME to "Draft is in the shared folder",
      SAM to "Read it, two small comments",
      ME to "Fixed both",
      SAM to "Looks good to me",
      ME to "Sending it out this afternoon",
      SAM to "Thanks!",
      ME to "Done, it went out",
    )
    return lines.mapIndexed { i, (author, body) ->
      Message(id = "f$i", conversation = CONVERSATION, author = author, sentAt = start + i * MINUTE_MILLIS, body = body, reactions = emptyList())
    }
  }

  private fun extras(): Map<String, MessageExtras> {
    val files = imageFiles().associateBy { it.message }
    return mapOf(
      "2" to MessageExtras(message = "2", files = listOfNotNull(files["2"])),
      "3" to MessageExtras(message = "3", files = listOfNotNull(files["3"])),
      "5" to MessageExtras(
        message = "5",
        files = listOf(
          FileInfo(CONVERSATION, "5", "f-pdf", "Site report.pdf", "application/pdf", 482_000, null, FileTransfer("remote")),
        ),
      ),
    )
  }

  private fun imageFiles(): List<FileInfo> = listOf(
    FileInfo(
      CONVERSATION,
      "2",
      "f-portrait",
      "IMG_2041.jpg",
      "image/jpeg",
      3_400_000,
      ImageBytes(samplePhoto(THUMBNAIL_PX * 3 / 4, THUMBNAIL_PX)),
      FileTransfer("remote"),
    ),
    FileInfo(
      CONVERSATION,
      "3",
      "f-landscape",
      "IMG_2042.jpg",
      "image/jpeg",
      2_900_000,
      ImageBytes(samplePhoto(THUMBNAIL_PX, THUMBNAIL_PX * 9 / 16)),
      FileTransfer("remote"),
    ),
  )

  private companion object {
    const val ME = "1"
    const val SAM = "2"
    val SNAPSHOT = ChatSnapshot(
      me = User(id = ME, username = "alex", displayName = "Alex Example"),
      users = listOf(User(id = SAM, username = "sam", displayName = "Sam Doe")),
      conversations = listOf(Conversation(id = CONVERSATION, kind = "Direct", name = "Sam Doe", memberIds = listOf(ME, SAM))),
    )
  }
}

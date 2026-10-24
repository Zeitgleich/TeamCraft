package de.zeitgleich.teamcraft.ui.screens

import android.content.pm.ActivityInfo
import android.content.res.Configuration
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.LinearGradient
import android.graphics.Paint
import android.graphics.Shader
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Scaffold
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalConfiguration
import androidx.lifecycle.ViewModelProvider
import androidx.test.core.app.ActivityScenario
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.io.ByteArrayOutputStream
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
import de.zeitgleich.teamcraft.ui.MainActivity
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.theme.TeamCraftTheme

private const val MINUTE_MILLIS = 60_000L
/** Long side of the sample thumbnails, as senders make them since #315. */
private const val THUMBNAIL_PX = 640
private const val CONVERSATION = "c1"

/**
 * Screenshots of a conversation (#315, #309) in light and dark: image previews, the composer, the
 * attach sheet, the image viewer and landscape. Placeholder data only. Like [MainScreenScreenshots]
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
        AttachSheet(rememberAttachmentPickers(viewModel) {}, onDismiss = {})
      }
    }
    shoot("viewer-dark", isDark = true) { viewModel ->
      ConversationSample(viewModel)
      ImageViewer(images = imageFiles(), startFileId = "f-portrait", onDownload = {}, onDismiss = {})
    }
    shoot("conversation-landscape-light", isDark = false, isLandscape = true) { viewModel -> ConversationSample(viewModel) }
  }

  private fun shoot(
    name: String,
    isDark: Boolean,
    isLandscape: Boolean = false,
    content: @Composable (MainViewModel) -> Unit,
  ) {
    val instrumentation = InstrumentationRegistry.getInstrumentation()
    ActivityScenario.launch(MainActivity::class.java).use { scenario ->
      scenario.onActivity { activity ->
        if (isLandscape) activity.requestedOrientation = ActivityInfo.SCREEN_ORIENTATION_LANDSCAPE
      }
      instrumentation.waitForIdleSync()
      scenario.onActivity { activity ->
        val viewModel = ViewModelProvider(activity)[MainViewModel::class.java]
        activity.setContent {
          val configuration = Configuration(LocalConfiguration.current).apply {
            uiMode = (uiMode and Configuration.UI_MODE_NIGHT_MASK.inv()) or
              (if (isDark) Configuration.UI_MODE_NIGHT_YES else Configuration.UI_MODE_NIGHT_NO)
          }
          CompositionLocalProvider(LocalConfiguration provides configuration) {
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

  /** The conversation screen's parts with sample data (the screen itself reads the core). */
  @Composable
  private fun ConversationSample(viewModel: MainViewModel) {
    Scaffold(
      topBar = { ChromeTopBar(title = "Sam Doe", maybeOnBack = {}) },
      contentWindowInsets = ScreenInsets,
    ) { padding ->
      Column(Modifier.fillMaxSize().padding(padding)) {
        MessageFeed(
          viewModel = viewModel,
          snapshot = SNAPSHOT,
          messages = messages(),
          extras = extras(),
          maybeFirstUnread = null,
          onOpenThread = {},
          maybeOnReply = {},
          onLongPress = {},
          modifier = Modifier.weight(1f),
        )
        Composer(viewModel, CONVERSATION, maybeThread = null, snapshot = SNAPSHOT)
      }
    }
  }

  private fun messages(): List<Message> {
    val start = System.currentTimeMillis() - 30 * MINUTE_MILLIS
    fun message(id: String, author: String, minute: Int, body: String, reactions: List<Reaction> = emptyList()) =
      Message(id = id, conversation = CONVERSATION, author = author, sentAt = start + minute * MINUTE_MILLIS, body = body, reactions = reactions)
    return listOf(
      message("1", SAM, 0, "Here are the photos from the site visit"),
      message("2", SAM, 1, ""),
      message("3", SAM, 2, ""),
      message("4", ME, 6, "Great, thanks! The second one is perfect for the report", listOf(Reaction("👍", 1, false))),
      message("5", SAM, 9, ""),
      message("6", ME, 12, "Sending the draft in a minute"),
    )
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
      CONVERSATION, "2", "f-portrait", "IMG_2041.jpg", "image/jpeg", 3_400_000,
      ImageBytes(samplePhoto(THUMBNAIL_PX * 3 / 4, THUMBNAIL_PX)), FileTransfer("remote"),
    ),
    FileInfo(
      CONVERSATION, "3", "f-landscape", "IMG_2042.jpg", "image/jpeg", 2_900_000,
      ImageBytes(samplePhoto(THUMBNAIL_PX, THUMBNAIL_PX * 9 / 16)), FileTransfer("remote"),
    ),
  )

  /** A drawn stand-in for a photo: sky, sun and hills, as a JPEG like senders make. */
  private fun samplePhoto(width: Int, height: Int): ByteArray {
    val bitmap = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888)
    val canvas = Canvas(bitmap)
    val paint = Paint(Paint.ANTI_ALIAS_FLAG)
    paint.shader = LinearGradient(0f, 0f, 0f, height.toFloat(), 0xFF3B7DD8.toInt(), 0xFFF2C38B.toInt(), Shader.TileMode.CLAMP)
    canvas.drawRect(0f, 0f, width.toFloat(), height.toFloat(), paint)
    paint.shader = null
    paint.color = 0xFFFFE08A.toInt()
    canvas.drawCircle(width * 0.7f, height * 0.3f, width * 0.12f, paint)
    paint.color = 0xFF2E6B3A.toInt()
    canvas.drawCircle(width * 0.2f, height * 1.1f, width * 0.6f, paint)
    paint.color = 0xFF3F8A4C.toInt()
    canvas.drawCircle(width * 0.9f, height * 1.2f, width * 0.7f, paint)
    return ByteArrayOutputStream().also { bitmap.compress(Bitmap.CompressFormat.JPEG, 80, it) }.toByteArray()
  }

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

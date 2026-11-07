package de.zeitgleich.teamcraft.ui.screens

import android.content.pm.ActivityInfo
import android.content.res.Configuration
import android.graphics.Bitmap
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Scaffold
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
import de.zeitgleich.teamcraft.ui.theme.TeamCraftTheme

private const val MINUTE_MILLIS = 60_000L
private const val CONVERSATION = "c1"

/** Conversation screen with markdown, long links, code, images and grouped messages (#704). */
@RunWith(AndroidJUnit4::class)
class PolishScreenshots {
  @Test
  fun polish() {
    assumeTrue(InstrumentationRegistry.getArguments().getString("screenshots") == "polish")
    shoot("polish-light", false, 1f)
    shoot("polish-dark", true, 1f)
    shoot("polish-text-light", false, 1f, textOnly = true)
    shoot("polish-text-dark", true, 1f, textOnly = true)
    shoot("polish-text-light-font13", false, 1.3f, textOnly = true)
    shoot("polish-text-dark-font20", true, 2f, textOnly = true)
    shoot("polish-light-font13", false, 1.3f)
  }

  private fun shoot(name: String, isDark: Boolean, fontScale: Float, textOnly: Boolean = false) {
    val instrumentation = InstrumentationRegistry.getInstrumentation()
    ActivityScenario.launch(MainActivity::class.java).use { scenario ->
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
            LocalDensity provides Density(density.density, fontScale),
          ) {
            TeamCraftTheme { Sample(viewModel, textOnly) }
          }
        }
      }
      instrumentation.waitForIdleSync()
      Thread.sleep(4_000)
      val bitmap = instrumentation.uiAutomation.takeScreenshot()
      val directory = File(instrumentation.targetContext.getExternalFilesDir(null), "screenshots").apply { mkdirs() }
      File(directory, "$name.png").outputStream().use { bitmap.compress(Bitmap.CompressFormat.PNG, 100, it) }
    }
  }

  @Composable
  private fun Sample(viewModel: MainViewModel, textOnly: Boolean) {
    Scaffold(
      topBar = {
        ConversationTopBar(SNAPSHOT, SNAPSHOT.conversations.first(), null, null, onBack = {}, actions = {})
      },
      contentWindowInsets = ScreenInsets,
    ) { padding ->
      Column(Modifier.fillMaxSize().padding(padding)) {
        MessageFeed(
          viewModel = viewModel,
          snapshot = SNAPSHOT,
          messages = messages().filter { !textOnly || it.body.isNotEmpty() },
          extras = extras(),
          firstUnread = FirstUnread.None,
          sentCount = 0,
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
    fun m(id: String, author: String, minute: Int, body: String) =
      Message(id = id, conversation = CONVERSATION, author = author, sentAt = start + minute * MINUTE_MILLIS, body = body)
    return listOf(
      m("1", SAM, 0, "Try `curl -s https://example.com/api` first, it is **fast** and _small_."),
      m("2", SAM, 0, "Then this:\n```\nfor i in 1 2 3; do\n  echo \$i some quite long line of code that should scroll sideways\ndone\n```"),
      m("3", SAM, 1, "> quoted line from the docs\nsee https://example.com/documentation/very/long/path/segments/that/go/on/and/on/report-final-v2.pdf?token=abcdef0123456789&other=1 now"),
      m("4", ME, 3, "Thanks! Reading **now**, `inline` code and a [link](https://example.com/x)."),
      m("5", ME, 3, "Second one in a row"),
      m("6", SAM, 5, ""),
      m("7", SAM, 6, ""),
    )
  }

  private fun extras(): Map<String, MessageExtras> {
    val files = listOf(
      FileInfo(CONVERSATION, "6", "f1", "a.jpg", "image/jpeg", 3_000_000, ImageBytes(samplePhoto(480, 640)), FileTransfer("remote")),
      FileInfo(CONVERSATION, "7", "f2", "b.jpg", "image/jpeg", 3_000_000, ImageBytes(samplePhoto(640, 360)), FileTransfer("remote")),
    )
    return files.associate { it.message to MessageExtras(message = it.message, files = listOf(it)) }
  }

  private companion object {
    const val ME = "1"
    const val SAM = "2"
    val SNAPSHOT = ChatSnapshot(
      me = User(id = ME, username = "alex", displayName = "Alex Example"),
      users = listOf(User(id = SAM, username = "sam", displayName = "Sam Doe", presence = "Online")),
      conversations = listOf(Conversation(id = CONVERSATION, kind = "Direct", name = "Sam Doe", memberIds = listOf(ME, SAM))),
    )
  }
}

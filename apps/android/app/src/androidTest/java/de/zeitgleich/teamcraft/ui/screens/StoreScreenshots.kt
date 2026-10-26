package de.zeitgleich.teamcraft.ui.screens

import android.content.res.Configuration
import android.graphics.Bitmap
import androidx.activity.compose.setContent
import androidx.compose.foundation.Image
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.res.stringResource
import androidx.lifecycle.ViewModelProvider
import androidx.test.core.app.ActivityScenario
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import java.util.Locale
import org.junit.Assume.assumeTrue
import org.junit.Test
import org.junit.runner.RunWith
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.calls.CallUiActions
import de.zeitgleich.teamcraft.calls.CallUiState
import de.zeitgleich.teamcraft.calls.InCallContent
import de.zeitgleich.teamcraft.core.ChatSnapshot
import de.zeitgleich.teamcraft.core.Conversation
import de.zeitgleich.teamcraft.core.FileInfo
import de.zeitgleich.teamcraft.core.FileTransfer
import de.zeitgleich.teamcraft.core.ImageBytes
import de.zeitgleich.teamcraft.core.Message
import de.zeitgleich.teamcraft.core.MessageExtras
import de.zeitgleich.teamcraft.core.Reaction
import de.zeitgleich.teamcraft.core.User
import de.zeitgleich.teamcraft.ui.AppState
import de.zeitgleich.teamcraft.ui.ChatListEntry
import de.zeitgleich.teamcraft.ui.ChatPreview
import de.zeitgleich.teamcraft.ui.ChatTab
import de.zeitgleich.teamcraft.ui.MainActivity
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.PreviewContent
import de.zeitgleich.teamcraft.ui.PreviewSender
import de.zeitgleich.teamcraft.ui.theme.TeamCraftTheme
import uniffi.teamcraft_ffi.CallPeer
import uniffi.teamcraft_ffi.CallPhase
import uniffi.teamcraft_ffi.CallStateInfo
import uniffi.teamcraft_ffi.ConversationItem
import uniffi.teamcraft_ffi.ConversationKind
import uniffi.teamcraft_ffi.Presence
import uniffi.teamcraft_ffi.VideoTrackInfo

private const val MINUTE_MILLIS = 60_000L
private const val HOUR_MILLIS = 60 * MINUTE_MILLIS
private const val CHANNEL = "c-design"
private const val THREAD_ROOT = "m3"
private const val ME = "u1"

/**
 * The Google Play listing's phone screenshots (apps/android/store, issue #479): chat list, a
 * channel, a thread, the spaces, joining a call and the settings, with placeholder people and texts.
 * Not a check: it runs only when asked for (`-e screenshots store`), in the language given by
 * `-e locale en|de`, and writes PNGs to the app's external files directory (`store/<locale>/`).
 * scripts/screenshots/android-store.sh runs it on its own emulator and pulls the pictures.
 */
@RunWith(AndroidJUnit4::class)
class StoreScreenshots {
  private val arguments = InstrumentationRegistry.getArguments()
  private val language = arguments.getString("locale") ?: "en"
  private val text = if (language == "de") GERMAN else ENGLISH

  @Test
  fun storeListing() {
    assumeTrue(arguments.getString("screenshots") == "store")
    val isCallWanted = arguments.getString("call") == "true"
    val shots = listOfNotNull(
      Shot("chats") { ChatsSample(ChatTab.Home) },
      Shot("channel") { viewModel -> ChannelSample(viewModel) },
      Shot("thread") { viewModel -> ThreadSample(viewModel) },
      Shot("spaces") { ChatsSample(ChatTab.Spaces) },
      // The call screen is being redesigned (#497); its picture is only taken with `-e call true`.
      Shot("call") { CallSample() }.takeIf { isCallWanted },
      Shot("join-call") { viewModel ->
        ChannelSample(viewModel)
        PreJoinDialog(text.channelName, onDismiss = {}, onJoin = {})
      },
      Shot("settings", isDark = false) { viewModel ->
        SettingsScreen(viewModel, AppState.Ready(ME, ALEX, identityFingerprint = ""), onBack = {})
      },
    )
    shots.forEachIndexed { index, shot -> shoot("${index + 1}-${shot.name}", shot.isDark, shot.content) }
  }

  private fun shoot(name: String, isDark: Boolean, content: @Composable (MainViewModel) -> Unit) {
    val instrumentation = InstrumentationRegistry.getInstrumentation()
    // The system's night mode, so the status bar's icons match the screen (the content follows
    // the override below either way).
    shell("cmd uimode night ${if (isDark) "yes" else "no"}")
    // Not timing a result: SystemUI redraws the status bar after a night mode change; demo mode
    // set before that is lost.
    Thread.sleep(1_500)
    cleanStatusBar()
    ActivityScenario.launch(MainActivity::class.java).use { scenario ->
      instrumentation.waitForIdleSync()
      scenario.onActivity { activity ->
        val locale = Locale.forLanguageTag(language)
        Locale.setDefault(locale)
        val localized = Configuration(activity.resources.configuration).apply { setLocale(locale) }
        // Deprecated, but the one way to switch this activity's own resources (which Compose's
        // string lookups read) without restarting it under a per-app locale.
        @Suppress("DEPRECATION")
        activity.resources.updateConfiguration(localized, activity.resources.displayMetrics)
        val viewModel = ViewModelProvider(activity)[MainViewModel::class.java]
        activity.setContent {
          val configuration = Configuration(LocalConfiguration.current).apply {
            setLocale(locale)
            uiMode = (uiMode and Configuration.UI_MODE_NIGHT_MASK.inv()) or
              (if (isDark) Configuration.UI_MODE_NIGHT_YES else Configuration.UI_MODE_NIGHT_NO)
          }
          CompositionLocalProvider(LocalConfiguration provides configuration) {
            TeamCraftTheme { content(viewModel) }
          }
        }
      }
      instrumentation.waitForIdleSync()
      // Not timing a result: lets decoding, the dialog's animation and fonts settle before the picture.
      Thread.sleep(4_000)
      val bitmap = instrumentation.uiAutomation.takeScreenshot()
      val directory = File(instrumentation.targetContext.getExternalFilesDir(null), "store/$language").apply { mkdirs() }
      File(directory, "$name.png").outputStream().use { bitmap.compress(Bitmap.CompressFormat.PNG, 100, it) }
    }
  }

  /**
   * SystemUI's demo mode: a fixed time, full battery and Wi-Fi, no notification icons. Set again
   * for every picture, as switching the night mode redraws the status bar without it.
   */
  private fun cleanStatusBar() {
    shell("settings put global sysui_demo_allowed 1")
    // Not timing a result: SystemUI reads the setting asynchronously and ignores demo commands
    // until it has.
    Thread.sleep(1_000)
    for (command in DEMO_COMMANDS) shell("am broadcast -a com.android.systemui.demo -e command $command")
  }

  /** Runs a shell command on the device and waits for it to finish. */
  private fun shell(command: String) {
    val output = InstrumentationRegistry.getInstrumentation().uiAutomation.executeShellCommand(command)
    android.os.ParcelFileDescriptor.AutoCloseInputStream(output).use { it.readBytes() }
  }

  @Composable
  private fun ChatsSample(tab: ChatTab) {
    MainScaffold(
      brand = MainBrand(stringResource(R.string.app_name), maybeLogo = null),
      accountName = ALEX,
      tab = tab,
      onTab = {},
      maybeConversations = chatItems(),
      list = ListState(
        previews = chatPreviews(),
        maybeSelectedId = null,
        selection = emptyList(),
        actionsFor = { _, _ -> RowActions(isUnread = false, toggleRead = null, toggleFavorite = {}) },
        onOpen = {},
        onLongPress = {},
      ),
      maybeSelectionBar = null,
      onSearch = {},
      onAccount = {},
      onNavigate = {},
    )
  }

  @Composable
  private fun ChannelSample(viewModel: MainViewModel) {
    Scaffold(
      topBar = { ChromeTopBar(title = "# ${text.channelName}", maybeSubtitle = text.channelTopic, maybeOnBack = {}) },
      contentWindowInsets = ScreenInsets,
    ) { padding ->
      Column(Modifier.fillMaxSize().padding(padding)) {
        MessageFeed(
          viewModel = viewModel,
          snapshot = snapshot(),
          messages = channelMessages(),
          extras = channelExtras(),
          maybeFirstUnread = null,
          onOpenThread = {},
          maybeOnReply = {},
          onLongPress = {},
          modifier = Modifier.weight(1f),
        )
        Composer(viewModel, CHANNEL, maybeThread = null, snapshot = snapshot())
      }
    }
  }

  @Composable
  private fun ThreadSample(viewModel: MainViewModel) {
    Scaffold(
      topBar = {
        ChromeTopBar(
          title = stringResource(R.string.thread_title),
          maybeSubtitle = "# ${text.channelName}",
          maybeOnBack = {},
          actions = { TextButton(onClick = {}) { Text(stringResource(R.string.thread_unfollow)) } },
        )
      },
      contentWindowInsets = ScreenInsets,
    ) { padding ->
      Column(Modifier.fillMaxSize().padding(padding)) {
        MessageFeed(
          viewModel = viewModel,
          snapshot = snapshot(),
          messages = threadMessages(),
          extras = emptyMap(),
          maybeFirstUnread = null,
          onOpenThread = null,
          maybeOnReply = null,
          onLongPress = {},
          modifier = Modifier.weight(1f),
        )
        Composer(viewModel, CHANNEL, maybeThread = THREAD_ROOT, snapshot = snapshot())
      }
    }
  }

  /** The call screen's parts (the screen itself reads the running call) with six camera tiles. */
  @Composable
  private fun CallSample() {
    val tracks = (1..6).map { VideoTrackInfo(streamId = it.toUInt(), deviceId = "d$it", isScreen = false) }
    val frames = tracks.associate { track -> track.streamId to sampleCameraFrame(640, 480, hue = track.streamId.toFloat() * 57f) }
    val state = CallStateInfo(
      conversationId = CHANNEL,
      phase = CallPhase.CONNECTED,
      isMuted = false,
      isDeafened = false,
      isCameraOn = true,
      isSecured = true,
      participantCount = 6u,
      participantUserIds = PEOPLE.map { it.first },
      peers = tracks.zip(PEOPLE).map { (track, person) -> CallPeer(deviceId = track.deviceId, userId = person.first) },
      speakingUserIds = listOf(PEOPLE[1].first),
      mutedUserIds = listOf(PEOPLE[2].first),
      reconnectingCount = 0u,
      reconnectingUserIds = emptyList(),
      maybePrivacyCode = "4821 9035 1176",
      maybeEndReason = null,
    )
    InCallContent(
      ui = CallUiState(
        name = text.channelName,
        maybeState = state,
        tracks = tracks,
        frames = frames,
        isCameraOn = true,
        isSpeakerOn = true,
        elapsedSeconds = 754L,
        areControlsShown = true,
        names = PEOPLE.associate { (id, name) -> id to name },
      ),
      actions = CallUiActions(
        onToggleControls = {},
        onInteraction = {},
        onMute = {},
        onDeafen = {},
        onCamera = {},
        onFlipCamera = {},
        onSpeaker = {},
        maybeOnAddPeople = null,
        onHangUp = {},
      ),
      selfView = { modifier ->
        Image(
          bitmap = sampleCameraFrame(480, 640, hue = 215f).asImageBitmap(),
          contentDescription = null,
          contentScale = ContentScale.Crop,
          modifier = modifier,
        )
      },
    )
  }

  private fun snapshot() = ChatSnapshot(
    me = User(id = ME, username = "alex", displayName = ALEX, presence = "Online", avatarHue = 215),
    users = PEOPLE.map { (id, name, hue) -> User(id = id, username = name.lowercase().substringBefore(' '), displayName = name, presence = "Online", avatarHue = hue) },
    conversations = listOf(
      Conversation(id = CHANNEL, kind = "Public", name = text.channelName, topic = text.channelTopic, memberIds = listOf(ME) + PEOPLE.map { it.first }),
    ),
  )

  private fun channelMessages(): List<Message> {
    val start = System.currentTimeMillis() - 40 * MINUTE_MILLIS
    fun message(id: String, author: String, minute: Int, body: String, reactions: List<Reaction> = emptyList(), replies: Int = 0) =
      Message(id = id, conversation = CHANNEL, author = author, sentAt = start + minute * MINUTE_MILLIS, body = body, reactions = reactions, replyCount = replies)
    return listOf(
      message("m1", SAM, 0, text.channel[0]),
      message("m2", SAM, 1, ""),
      message("m3", KIM, 5, text.channel[1], listOf(Reaction("🎉", 3), Reaction("👍", 2, reactedByMe = true)), replies = 4),
      message("m4", JO, 12, text.channel[2], listOf(Reaction("❤️", 2))),
      message("m5", ME, 18, text.channel[3]),
      message("m6", SAM, 21, text.channel[4], listOf(Reaction("🙌", 1))),
    )
  }

  private fun channelExtras(): Map<String, MessageExtras> = mapOf(
    "m2" to MessageExtras(
      message = "m2",
      files = listOf(
        FileInfo(
          CHANNEL, "m2", "f-mockup", "Homepage-v3.jpg", "image/jpeg", 2_400_000,
          ImageBytes(samplePhoto(640, 400)), FileTransfer("remote"),
        ),
      ),
    ),
  )

  private fun threadMessages(): List<Message> {
    val start = System.currentTimeMillis() - 35 * MINUTE_MILLIS
    fun reply(id: String, author: String, minute: Int, body: String, reactions: List<Reaction> = emptyList()) =
      Message(id = id, conversation = CHANNEL, author = author, sentAt = start + minute * MINUTE_MILLIS, body = body, reactions = reactions, maybeThread = THREAD_ROOT)
    return listOf(
      Message(id = THREAD_ROOT, conversation = CHANNEL, author = KIM, sentAt = start, body = text.channel[1], reactions = listOf(Reaction("🎉", 3))),
      reply("r1", SAM, 2, text.thread[0]),
      reply("r2", KIM, 4, text.thread[1], listOf(Reaction("👍", 2))),
      reply("r3", JO, 9, text.thread[2]),
      reply("r4", KIM, 12, text.thread[3], listOf(Reaction("👀", 1))),
      reply("r5", ME, 14, text.thread[4], listOf(Reaction("✅", 3))),
    )
  }

  private fun chatItems(): List<ConversationItem> = listOf(
    item("1", "Sam Doe", ConversationKind.DIRECT, unread = 2u, presence = Presence.ONLINE),
    item(CHANNEL, text.channelName, ConversationKind.PUBLIC, space = "Project A", isFavorite = true, unread = 6u, mentions = 1u),
    item("3", "Jo, Kim, Sam", ConversationKind.GROUP, isFavorite = true),
    item("4", text.releaseChannel, ConversationKind.PRIVATE, space = "Project A", isFavorite = true, unread = 1u),
    item("5", "Kim Lee", ConversationKind.DIRECT, presence = Presence.AWAY),
    item("6", "general", ConversationKind.PUBLIC, space = "Project B"),
    item("7", "Jo Roe", ConversationKind.DIRECT, presence = Presence.OFFLINE),
  )

  private fun chatPreviews(): Map<String, ChatListEntry> {
    val now = System.currentTimeMillis()
    fun entry(body: String, sender: PreviewSender, ageMinutes: Long) =
      ChatListEntry(ChatPreview(sender, PreviewContent.Text(body), now - ageMinutes * MINUTE_MILLIS), "m")
    return mapOf(
      "1" to entry(text.previews[0], PreviewSender.None, 2),
      CHANNEL to entry(text.channel[4], PreviewSender.Other("Sam"), 19),
      "3" to entry(text.previews[1], PreviewSender.Me, 55),
      "4" to entry(text.previews[2], PreviewSender.Other("Jo"), 90),
      "5" to entry(text.previews[3], PreviewSender.None, 3 * 60),
      "6" to entry(text.previews[4], PreviewSender.Other("Kim"), 26 * 60),
      "7" to entry(text.previews[5], PreviewSender.Me, 50 * HOUR_MILLIS / MINUTE_MILLIS),
    )
  }

  private fun item(
    id: String,
    name: String,
    kind: ConversationKind,
    space: String? = null,
    isFavorite: Boolean = false,
    unread: UInt = 0u,
    mentions: UInt = 0u,
    presence: Presence? = null,
  ) = ConversationItem(
    id = id,
    maybeSpaceId = space?.let { "s-$it" },
    maybeSpaceName = space,
    kind = kind,
    name = name,
    topic = "",
    unreadCount = unread,
    mentionCount = mentions,
    isFavorite = isFavorite,
    isMuted = false,
    canPost = true,
    maybePeerPresence = presence,
  )

  /** One picture: its file name (without the number), its theme and what it shows. */
  private class Shot(val name: String, val isDark: Boolean = true, val content: @Composable (MainViewModel) -> Unit)

  /** The sample conversation's texts in one language. */
  private class SampleTexts(
    val channelName: String,
    val channelTopic: String,
    val releaseChannel: String,
    val channel: List<String>,
    val thread: List<String>,
    val previews: List<String>,
  )

  private companion object {
    const val ALEX = "Alex Doe"
    const val SAM = "u2"
    const val KIM = "u3"
    const val JO = "u4"
    val PEOPLE = listOf(Triple(SAM, "Sam Doe", 150), Triple(KIM, "Kim Lee", 20), Triple(JO, "Jo Roe", 280))
    val DEMO_COMMANDS = listOf(
      "enter",
      "clock -e hhmm 0941",
      "battery -e level 100 -e plugged false",
      "network -e wifi show -e level 4 -e fully true",
      "network -e mobile show -e datatype none -e level 4 -e fully true",
      "notifications -e visible false",
    )

    // Store screenshots for German users show German chats; like the i18n files, the only
    // non-English text here.
    val ENGLISH = SampleTexts(
      channelName = "design",
      channelTopic = "Website relaunch, Project A",
      releaseChannel = "releases",
      channel = listOf(
        "New homepage mockup is ready, feedback welcome!",
        "Looks great. The hero section finally breathes. Let's ship the beta on Thursday?",
        "Agreed. I'll update the icons in the footer today.",
        "I booked a review call for 3 pm, link is in the meeting invite.",
        "Perfect, see you there 👋",
      ),
      thread = listOf(
        "Thursday works for me. QA can start tomorrow morning.",
        "Great, I'll write the release notes.",
        "Copy for the newsletter is in the shared folder.",
        "Can we add the accessibility improvements too?",
        "Added. Thanks all, I'll tag the build tonight.",
      ),
      previews = listOf(
        "Can you look at the draft before noon?",
        "Thanks, see you tomorrow",
        "Release notes are up",
        "I moved the meeting to 3 pm",
        "Lunch at 12?",
        "Sounds good!",
      ),
    )

    val GERMAN = SampleTexts(
      channelName = "design",
      channelTopic = "Website-Relaunch, Projekt A",
      releaseChannel = "releases",
      channel = listOf(
        "Der neue Entwurf der Startseite ist fertig, Feedback willkommen!",
        "Sieht super aus. Der Kopfbereich wirkt endlich luftig. Beta am Donnerstag?",
        "Einverstanden. Ich passe heute noch die Icons im Footer an.",
        "Ich habe für 15 Uhr einen Review-Call angelegt, der Link steht in der Einladung.",
        "Perfekt, bis gleich 👋",
      ),
      thread = listOf(
        "Donnerstag passt. Die QA kann morgen früh starten.",
        "Super, ich schreibe die Release Notes.",
        "Der Text für den Newsletter liegt im geteilten Ordner.",
        "Können wir die Verbesserungen bei der Barrierefreiheit noch ergänzen?",
        "Ergänzt. Danke euch, ich tagge den Build heute Abend.",
      ),
      previews = listOf(
        "Kannst du vor Mittag auf den Entwurf schauen?",
        "Danke, bis morgen",
        "Die Release Notes sind online",
        "Ich habe das Meeting auf 15 Uhr verschoben",
        "Mittagessen um 12?",
        "Klingt gut!",
      ),
    )
  }
}

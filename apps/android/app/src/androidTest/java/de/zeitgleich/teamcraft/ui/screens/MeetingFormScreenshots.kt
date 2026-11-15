package de.zeitgleich.teamcraft.ui.screens

import android.content.res.Configuration
import android.graphics.Bitmap
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModelProvider
import androidx.test.core.app.ActivityScenario
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import de.zeitgleich.teamcraft.core.Meeting
import de.zeitgleich.teamcraft.ui.MainActivity
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.theme.TeamCraftTheme
import org.junit.Assume.assumeTrue
import org.junit.Test
import org.junit.runner.RunWith
import uniffi.teamcraft_ffi.MeetingExternalInvitee
import uniffi.teamcraft_ffi.MeetingForm
import uniffi.teamcraft_ffi.MeetingFormProblem
import uniffi.teamcraft_ffi.MeetingRepeat
import uniffi.teamcraft_ffi.MeetingRepeatForm
import uniffi.teamcraft_ffi.MeetingSeriesEnd
import uniffi.teamcraft_ffi.SavedMeeting
import java.io.File
import java.time.LocalDate

/**
 * The schedule form's parts that came with desktop parity (#1905): co-hosts, the notetaker, e-mail
 * invitees and the repeat rule, filled and with errors at their fields. Run with
 * `-e screenshots meeting-form`.
 */
@RunWith(AndroidJUnit4::class)
class MeetingFormScreenshots {
  @Test
  fun meetingForm() {
    assumeTrue(InstrumentationRegistry.getArguments().getString("screenshots") == "meeting-form")
    shoot("meeting-form-light", isDark = false, sample(), emptyList())
    shoot("meeting-form-dark", isDark = true, sample(), emptyList())
    val broken = sample().let {
      it.copy(
        externalInvitees = it.externalInvitees + MeetingExternalInvitee("jordan@example", ""),
        repeatRule = it.repeatRule.copy(weekdays = List(7) { false }, count = 0u),
      )
    }
    val problems = listOf(
      MeetingFormProblem.InvalidEmail("jordan@example"),
      MeetingFormProblem.NoWeekday,
      MeetingFormProblem.InvalidCount,
    )
    shoot("meeting-form-errors-light", isDark = false, broken, problems)
  }

  /** "Your meeting is ready" with its link, and rows of a cancelled meeting and a skipped holiday. */
  @Test
  fun meetingLinks() {
    assumeTrue(InstrumentationRegistry.getArguments().getString("screenshots") == "meeting-links")
    shootContent("meeting-created-light", isDark = false) { LinksSample(it) }
    shootContent("meeting-created-dark", isDark = true) { LinksSample(it) }
  }

  private fun shoot(name: String, isDark: Boolean, form: MeetingForm, problems: List<MeetingFormProblem>) {
    shootContent(name, isDark) { Sample(form, problems) }
  }

  private fun shootContent(name: String, isDark: Boolean, content: @Composable (MainViewModel) -> Unit) {
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
          CompositionLocalProvider(LocalConfiguration provides configuration) {
            TeamCraftTheme { content(viewModel) }
          }
        }
      }
      instrumentation.waitForIdleSync()
      // Not timing a result: lets fonts and the first frame settle before the picture.
      Thread.sleep(3_000)
      val bitmap = instrumentation.uiAutomation.takeScreenshot()
      val directory = File(instrumentation.targetContext.getExternalFilesDir(null), "meeting-form").apply { mkdirs() }
      File(directory, "$name.png").outputStream().use { bitmap.compress(Bitmap.CompressFormat.PNG, 100, it) }
    }
  }

  @Composable
  private fun Sample(initial: MeetingForm, problems: List<MeetingFormProblem>) {
    var form by remember { mutableStateOf(initial) }
    val focus = remember { MeetingField.entries.associateWith { FocusRequester() } }
    val errorOf: (MeetingField) -> Int? = { field -> problems.firstOrNull { it.field == field }?.text }
    Surface(Modifier.fillMaxSize()) {
      Column(
        Modifier.verticalScroll(rememberScrollState()).padding(vertical = 48.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
      ) {
        Text("Weekly planning", style = MaterialTheme.typography.titleLarge, modifier = Modifier.padding(horizontal = 16.dp))
        NotetakerRow(form, "bot", isSaving = false) { form = it }
        CoHostsSection(form, NAMES, isSaving = false) { form = it }
        ExternalInviteesSection(form, problems, focus.getValue(MeetingField.EXTERNALS), isSaving = false) { form = it }
        HorizontalDivider()
        RepeatSection(form, isSavedSeries = false, errorOf, focus, isSaving = false) { form = it }
      }
    }
  }

  @Composable
  private fun LinksSample(viewModel: MainViewModel) {
    val start = System.currentTimeMillis() + 2 * HOUR_MS
    val saved = SavedMeeting("7", "8", "Weekly planning", start, start + HOUR_MS / 2, "")
    Surface(Modifier.fillMaxSize()) {
      Column(Modifier.verticalScroll(rememberScrollState()).padding(vertical = 48.dp)) {
        MeetingCreatedContent(
          saved = saved,
          link = LinkState.Ready("https://chat.example.com/meeting/AbCdEfGhIjKlMnOpQrSt#v=3f9a1c0b7e2d4a56"),
          isCopied = false,
          onRetry = {},
          onCopy = {},
          onDismiss = {},
          onJoin = {},
        )
        HorizontalDivider()
        MeetingRow(viewModel, Meeting("1", "Design review", start + DAY_MS, start + DAY_MS + HOUR_MS, "cancelled", "c1"))
        HorizontalDivider()
        MeetingRow(
          viewModel,
          Meeting("2", "Team sync", start + 2 * DAY_MS, start + 2 * DAY_MS + HOUR_MS, "cancelled", "c2", isOccurrenceHoliday = true, isOccurrenceCancelled = true),
        )
        HorizontalDivider()
      }
    }
  }

  private fun sample() = MeetingForm(
    title = "Weekly planning",
    description = "",
    startsAtMs = System.currentTimeMillis(),
    utcOffsetMinutes = 0,
    durationMinutes = 30u,
    inviteeUserIds = NAMES.keys.toList(),
    externalInvitees = listOf(MeetingExternalInvitee("guest@example.com", "Robin Guest")),
    repeatRule = MeetingRepeatForm(
      frequency = MeetingRepeat.WEEKLY,
      interval = 1u,
      weekdays = listOf(true, false, false, true, false, false, false),
      ends = MeetingSeriesEnd.AFTER_COUNT,
      count = 6u,
      maybeUntilMs = endOfDayMs(LocalDate.now().plusWeeks(6)),
      skipsHolidays = true,
    ),
    hasLobby = true,
    maybeNotetakerUserId = "bot",
    coHostUserIds = listOf("2"),
    maybeRequestKey = null,
  )

  private companion object {
    const val HOUR_MS = 3_600_000L
    const val DAY_MS = 24 * HOUR_MS
    val NAMES = mapOf("2" to "Alex Example", "3" to "Sam Sample")
  }
}

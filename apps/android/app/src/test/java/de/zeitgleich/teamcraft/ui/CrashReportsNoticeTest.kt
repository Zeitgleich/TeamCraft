package de.zeitgleich.teamcraft.ui

import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.crash.CrashReportsStatus
import de.zeitgleich.teamcraft.ui.screens.hintText
import de.zeitgleich.teamcraft.ui.screens.noticeText
import org.junit.Assert.assertEquals
import org.junit.Test

class CrashReportsNoticeTest {
  @Test
  fun theNoticeFollowsTheRecipientAndTheOptOut() {
    assertEquals(
      R.string.crash_reports_notice_publisher_optional,
      noticeText(CrashReportsStatus(allowsOptOut = true, showsNotice = true, isPublisherRecipient = true)),
    )
    assertEquals(
      R.string.crash_reports_notice_publisher_required,
      noticeText(CrashReportsStatus(allowsOptOut = false, showsNotice = true, isPublisherRecipient = true)),
    )
    assertEquals(
      R.string.crash_reports_notice_organization_optional,
      noticeText(CrashReportsStatus(allowsOptOut = true, showsNotice = true, isPublisherRecipient = false)),
    )
    assertEquals(
      R.string.crash_reports_notice_organization_required,
      noticeText(CrashReportsStatus(allowsOptOut = false, showsNotice = true, isPublisherRecipient = false)),
    )
  }

  @Test
  fun theSettingsHintNamesTheSameRecipient() {
    assertEquals(
      R.string.crash_reports_send_hint_publisher,
      hintText(CrashReportsStatus(allowsOptOut = true, showsNotice = true, isPublisherRecipient = true)),
    )
    assertEquals(
      R.string.crash_reports_required_hint_organization,
      hintText(CrashReportsStatus(allowsOptOut = false, showsNotice = true, isPublisherRecipient = false)),
    )
  }
}

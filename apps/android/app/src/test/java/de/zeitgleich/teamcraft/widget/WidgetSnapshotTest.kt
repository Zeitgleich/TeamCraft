package de.zeitgleich.teamcraft.widget

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.teamcraft_ffi.NotificationContent

class WidgetSnapshotTest {
  @Test
  fun theAppLockHidesEverythingWhateverTheOtherSettings() {
    for (content in NotificationContent.entries) {
      assertEquals(WidgetPrivacy.LOCKED, widgetPrivacy(content, isAppLockOn = true, isPreviewWanted = true))
    }
    val locked = widgetSnapshot(WidgetPrivacy.LOCKED, rows(3))
    assertTrue(locked.rows.isEmpty())
    assertEquals(0, locked.unreadConversations)
  }

  @Test
  fun notificationsThatShowNothingLeaveOnlyTheCount() {
    assertEquals(WidgetPrivacy.COUNT_ONLY, widgetPrivacy(NotificationContent.NOTHING, isAppLockOn = false, isPreviewWanted = true))
    val snapshot = widgetSnapshot(WidgetPrivacy.COUNT_ONLY, rows(3))
    assertTrue(snapshot.rows.isEmpty())
    assertEquals(3, snapshot.unreadConversations)
  }

  @Test
  fun namesAndCountsByDefaultWithoutPreviews() {
    assertEquals(WidgetPrivacy.NAMES, widgetPrivacy(NotificationContent.CONTENT, isAppLockOn = false, isPreviewWanted = false))
    assertEquals(WidgetPrivacy.NAMES, widgetPrivacy(NotificationContent.SENDER_ONLY, isAppLockOn = false, isPreviewWanted = true))
    val snapshot = widgetSnapshot(WidgetPrivacy.NAMES, rows(2))
    assertEquals(listOf("Conversation 0", "Conversation 1"), snapshot.rows.map { it.name })
    snapshot.rows.forEach { assertNull(it.maybePreview) }
  }

  @Test
  fun previewsOnlyWithContentNotificationsAndTheOptIn() {
    assertEquals(WidgetPrivacy.PREVIEWS, widgetPrivacy(NotificationContent.CONTENT, isAppLockOn = false, isPreviewWanted = true))
    val snapshot = widgetSnapshot(WidgetPrivacy.PREVIEWS, rows(2))
    assertEquals("Preview 1", snapshot.rows[1].maybePreview)
  }

  @Test
  fun aLongListKeepsItsCountButNotEveryRow() {
    val snapshot = widgetSnapshot(WidgetPrivacy.NAMES, rows(WIDGET_MAX_ROWS + 5))
    assertEquals(WIDGET_MAX_ROWS, snapshot.rows.size)
    assertEquals(WIDGET_MAX_ROWS + 5, snapshot.unreadConversations)
  }

  private fun rows(count: Int): List<WidgetRow> = (0 until count).map { index ->
    WidgetRow(conversationId = "${100 + index}", name = "Conversation $index", countLabel = "1", maybePreview = "Preview $index")
  }
}

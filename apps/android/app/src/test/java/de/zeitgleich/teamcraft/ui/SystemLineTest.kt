package de.zeitgleich.teamcraft.ui

import de.zeitgleich.teamcraft.core.ChatActions
import de.zeitgleich.teamcraft.core.Message
import de.zeitgleich.teamcraft.core.MessageExtras
import de.zeitgleich.teamcraft.core.WireJson
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** The lines system messages show (issue #769) and the read state of own messages. */
class SystemLineTest {
  private val names = mapOf("1" to "Ada", "2" to "Sam")

  @Test
  fun systemEventsNameTheirAuthor() {
    assertEquals(SystemLine.Joined("Sam"), line(kind = """{"System":"Joined"}"""))
    assertEquals(SystemLine.Left("Sam"), line(kind = """{"System":"Left"}"""))
    assertEquals(
      SystemLine.Renamed("Sam", "Project A"),
      line(kind = """{"System":{"Renamed":{"name":"Project A"}}}"""),
    )
    assertEquals(
      SystemLine.TopicChanged("Sam", "Launch"),
      line(kind = """{"System":{"TopicChanged":{"topic":"Launch"}}}"""),
    )
  }

  @Test
  fun whoAddedWhomWinsOverThePlainJoin() {
    // The core reports an addition as the added person's join, with the change in the extras.
    assertEquals(
      SystemLine.MemberAdded("Sam", "Ada", sharesHistory = true),
      line(
        kind = """{"System":"Joined"}""",
        workspace = """{"event":"member_added","user":"2","by":"1","history":"full"}""",
      ),
    )
    assertEquals(
      SystemLine.MemberRemoved("Sam", "Ada"),
      line(kind = """{"System":"Left"}""", workspace = """{"event":"member_removed","user":"2","by":"1"}"""),
    )
  }

  @Test
  fun channelChangesNameWhoMadeThem() {
    assertEquals(SystemLine.Archived("Ada"), line(workspace = """{"event":"archived","by":"1"}"""))
    assertEquals(SystemLine.Unarchived("Ada"), line(workspace = """{"event":"unarchived","by":"1"}"""))
    assertEquals(SystemLine.MadePublic("Ada"), line(workspace = """{"event":"made_public","by":"1"}"""))
    assertEquals(SystemLine.Pinned("Ada"), line(workspace = """{"event":"pinned","message":"9","by":"1"}"""))
    assertEquals(SystemLine.Moved("Ada", ""), line(workspace = """{"event":"moved","by":"1","space":""}"""))
  }

  @Test
  fun anAdminWhoJoinedSaysWhoApprovedTheRequest() {
    // admin.md §2.4: by themselves (`visible`), or let in by an owner (`owner_approval`, #799).
    assertEquals(
      SystemLine.AdminJoined("Sam", maybeApprovedBy = null),
      line(kind = """{"System":"Joined"}""", workspace = """{"event":"admin_joined","user":"2"}"""),
    )
    assertEquals(
      SystemLine.AdminJoined("Sam", maybeApprovedBy = "Ada"),
      line(workspace = """{"event":"admin_joined","user":"2","approved_by":"1"}"""),
    )
    // Added without an approval to ask for: "added by", not "approved by" (#811).
    assertEquals(
      SystemLine.AdminAdded("Sam", by = "Ada"),
      line(kind = "\"Change\"", workspace = """{"event":"admin_joined","user":"2","added_by":"1"}"""),
    )
  }

  @Test
  fun unknownEventsFallBackToAQuietLine() {
    assertEquals(
      SystemLine.Joined("Sam"),
      line(kind = """{"System":"Joined"}""", workspace = """{"event":"split_in_two","by":"1"}"""),
    )
    assertEquals(SystemLine.Changed, line(workspace = """{"event":"split_in_two","by":7}"""))
    assertEquals(SystemLine.Changed, line(kind = """{"System":"Vanished"}"""))
    assertEquals(SystemLine.Changed, line())
  }

  @Test
  fun writtenAndDeletedMessagesAreBubbles() {
    assertNull(line(body = "Hello"))
    assertNull(systemLine(message(), extras(""""is_deleted":true""")) { names[it].orEmpty() })
    assertNull(line(kind = """{"Log":"Info"}""", body = "build passed"))
  }

  @Test
  fun readStateAndItsRefresh() {
    assertTrue(extras(""""is_read":true""").isRead)
    assertFalse("older cores don't say: one tick", extras("").isRead)
    assertEquals(
      """{"action":"refresh_read_receipts","conversation":"7"}""",
      ChatActions.refreshReadReceipts("7"),
    )
  }

  private fun line(kind: String = "\"User\"", workspace: String? = null, body: String = ""): SystemLine? =
    systemLine(
      message(kind, body),
      workspace?.let { extras(""""maybe_workspace_event":$it""") },
    ) { names[it].orEmpty() }

  private fun message(kind: String = "\"User\"", body: String = ""): Message =
    WireJson.decodeFromString(
      Message.serializer(),
      """{"id":"9","conversation":"7","author":"2","kind":$kind,"sent_at":0,"body":"$body"}""",
    )

  /** Extras with [fields] (JSON members after the message ID). */
  private fun extras(fields: String): MessageExtras =
    WireJson.decodeFromString(
      MessageExtras.serializer(),
      """{"message":"9"${if (fields.isEmpty()) "" else ",$fields"}}""",
    )
}

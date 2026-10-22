package de.zeitgleich.teamcraft.core

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Test

/** The actions and queries must have the exact shape the core's serde types expect. */
class ChatActionsTest {
  @Test
  fun sendCarriesReplyPriorityAndFiles() {
    val json = Json.parseToJsonElement(
      ChatActions.send(
        "7",
        "hello",
        SendOptions(
          maybeReplyTo = "9",
          priority = "urgent",
          files = listOf(PickedFile("a.png", "image/png", "/cache/a.png")),
        ),
      ),
    ).jsonObject
    assertEquals("send_message", json["action"]!!.jsonPrimitive.content)
    val message = json["message"]!!.jsonObject
    assertEquals("7", message["conversation"]!!.jsonPrimitive.content)
    assertEquals(JsonNull, message["maybe_thread"])
    assertEquals("9", message["maybe_reply_to"]!!.jsonPrimitive.content)
    assertEquals("urgent", message["priority"]!!.jsonPrimitive.content)
    val file = message["files"]!!.jsonArray[0].jsonObject
    assertEquals("path", file["source"]!!.jsonObject["kind"]!!.jsonPrimitive.content)
    assertEquals("/cache/a.png", file["source"]!!.jsonObject["path"]!!.jsonPrimitive.content)
  }

  @Test
  fun actionsAndQueriesAreTagged() {
    assertEquals(
      """{"action":"toggle_reaction","conversation":"7","message":"9","emoji":"👍"}""",
      ChatActions.react("7", "9", "👍"),
    )
    assertEquals(
      """{"action":"set_thread_followed","conversation":"7","thread":"9","is_followed":true}""",
      ChatActions.setThreadFollowed("7", "9", true),
    )
    assertEquals("""{"action":"set_custom_status","maybe_status":null}""", ChatActions.setCustomStatus(null))
    assertEquals("""{"kind":"draft","conversation":"7","maybe_thread":null}""", ChatQueries.draft("7", null))
    assertEquals(
      """{"kind":"message_extras","conversation":"7","messages":["1","2"]}""",
      ChatQueries.messageExtras("7", listOf("1", "2")),
    )
    assertEquals("""{"kind":"followed_threads"}""", ChatQueries.followedThreads())
  }
}

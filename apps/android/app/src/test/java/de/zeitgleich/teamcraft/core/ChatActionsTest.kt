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
  /** The reply target is kept with the draft on this device (#1115). */
  @Test
  fun aReplyTargetIsKeptAndForgotten() {
    val kept = Json.parseToJsonElement(ChatActions.setDraftReply("7", "9", "4")).jsonObject
    assertEquals("set_draft_reply", kept["action"]!!.jsonPrimitive.content)
    assertEquals("9", kept["maybe_reply"]!!.jsonObject["message"]!!.jsonPrimitive.content)
    assertEquals("4", kept["maybe_reply"]!!.jsonObject["maybe_attachment"]!!.jsonPrimitive.content)

    val forgotten = Json.parseToJsonElement(ChatActions.setDraftReply("7", null, null)).jsonObject
    assertEquals(JsonNull, forgotten["maybe_reply"])
  }

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
    assertEquals(JsonNull, message["maybe_reply_attachment"])
    assertEquals("urgent", message["priority"]!!.jsonPrimitive.content)
    val file = message["files"]!!.jsonArray[0].jsonObject
    assertEquals("path", file["source"]!!.jsonObject["kind"]!!.jsonPrimitive.content)
    assertEquals("/cache/a.png", file["source"]!!.jsonObject["path"]!!.jsonPrimitive.content)
  }

  @Test
  fun aVideoCarriesItsPosterAndLength() {
    val video = PickedFile("v.mp4", "video/mp4", "/cache/v.mp4", maybeThumbnail = byteArrayOf(1, -1), maybeDurationMs = 2_500)
    val message = Json.parseToJsonElement(ChatActions.send("7", "", SendOptions(files = listOf(video)))).jsonObject["message"]!!.jsonObject
    val file = message["files"]!!.jsonArray[0].jsonObject
    // serde reads a `Vec<u8>` as an array of numbers 0..255.
    assertEquals(listOf("1", "255"), file["thumbnail"]!!.jsonArray.map { it.jsonPrimitive.content })
    assertEquals("2500", file["duration_ms"]!!.jsonPrimitive.content)
  }

  @Test
  fun theViewerDownloadsOneFileOfAMessage() {
    assertEquals(
      """{"action":"download_attachment","conversation":"7","message":"9","file_id":"12"}""",
      ChatActions.downloadAttachment("7", "9", "12"),
    )
  }

  @Test
  fun archiveChannelSetsOnlyTheArchivedChange() {
    assertEquals(
      """{"action":"update_channel","conversation":"7","changes":{"maybe_is_archived":true}}""",
      ChatActions.archiveChannel("7"),
    )
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

  @Test
  fun forwardNamesOneFileOnlyWhenAsked() {
    assertEquals(
      """{"action":"forward_message","from":"1","message":"2","to":"3","comment":""}""",
      ChatActions.forward("1", "2", "3", ""),
    )
    assertEquals(
      """{"action":"forward_message","from":"1","message":"2","to":"3","comment":"","attachment":"4"}""",
      ChatActions.forward("1", "2", "3", "", "4"),
    )
  }

  @Test
  fun replyNamesOnePicture() {
    val message = Json.parseToJsonElement(
      ChatActions.send("7", "this", SendOptions(maybeReplyTo = "9", maybeReplyAttachment = "4")),
    ).jsonObject["message"]!!.jsonObject
    assertEquals("4", message["maybe_reply_attachment"]!!.jsonPrimitive.content)
  }

  @Test
  fun listStatusAndRetryUseTheCoresNames() {
    assertEquals(
      """{"kind":"list_load","list":{"name":"call_history"}}""",
      ChatQueries.listLoad(LoadingList.CALL_HISTORY),
    )
    assertEquals(
      """{"action":"retry_list","list":{"name":"activity"}}""",
      ChatActions.retryList(LoadingList.ACTIVITY),
    )
  }

  @Test
  fun theListStatusAnswerReadsAllStatesAndUnknownCauses() {
    fun load(text: String) = decodeAnswer("""{"answer":"list_load","load":$text}""").load!!.toLoad()
    assertEquals(ListLoad.Loading, load("""{"state":"loading"}"""))
    assertEquals(ListLoad.Ready, load("""{"state":"ready"}"""))
    assertEquals(ListLoad.Failed(LoadFailure.TIMED_OUT), load("""{"state":"failed","failure":"timed_out"}"""))
    assertEquals(ListLoad.Failed(LoadFailure.OTHER), load("""{"state":"failed","failure":"from_the_future"}"""))
    // A state of a newer core is not waited for.
    assertEquals(ListLoad.Ready, load("""{"state":"paused"}"""))
  }
}

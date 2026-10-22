package de.zeitgleich.teamcraft.push

import org.junit.Assert.assertEquals
import org.junit.Test

class PushSubscriptionTest {
  @Test
  fun buildsTheWebPushSubscriptionJson() {
    assertEquals(
      """{"endpoint":"https://push.example.com/up/abc","keys":{"p256dh":"BPk","auth":"xyz"}}""",
      PushSubscription.json("https://push.example.com/up/abc", "BPk", "xyz"),
    )
  }

  @Test
  fun leavesOutMissingKeysAndEscapes() {
    assertEquals(
      """{"endpoint":"https://push.example.com/a\"b"}""",
      PushSubscription.json("https://push.example.com/a\"b", null, "xyz"),
    )
  }
}

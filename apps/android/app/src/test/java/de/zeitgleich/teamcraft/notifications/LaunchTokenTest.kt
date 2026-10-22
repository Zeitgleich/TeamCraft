package de.zeitgleich.teamcraft.notifications

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class LaunchTokenTest {
  @Test
  fun onlySnowflakesAreConversationIds() {
    assertTrue(LaunchToken.isConversationId("1234567890123456789"))
    for (id in listOf("", "12/../settings", "1?x=y", "-1", "١٢٣", "12345678901234567890")) {
      assertFalse(id, LaunchToken.isConversationId(id))
    }
  }
}

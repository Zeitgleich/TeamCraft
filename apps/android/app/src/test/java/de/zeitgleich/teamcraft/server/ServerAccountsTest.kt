package de.zeitgleich.teamcraft.server

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

private val TEAM = ServerAccount("https://team.example.com", "{\"origin\":\"https://team.example.com\"}")
private val OTHER = ServerAccount("https://other.example.com", "{\"origin\":\"https://other.example.com\"}")

class ServerAccountsTest {
  @Test
  fun savingMakesTheServerActiveAndReplacesTheSameOrigin() {
    val updated = TEAM.copy(profileJson = "{\"origin\":\"https://team.example.com\",\"v\":2}")
    val accounts = ServerAccounts().saved(TEAM).saved(OTHER).saved(updated)
    assertEquals(listOf(OTHER, updated), accounts.accounts)
    assertEquals(updated, accounts.maybeActive)
  }

  @Test
  fun forgettingTheActiveServerLeavesNoneActive() {
    val accounts = ServerAccounts().saved(OTHER).saved(TEAM).removed(TEAM.origin)
    assertEquals(listOf(OTHER), accounts.accounts)
    assertNull(accounts.maybeActive)
    assertEquals(OTHER, ServerAccounts().saved(TEAM).saved(OTHER).removed(TEAM.origin).maybeActive)
  }

  @Test
  fun roundTripsThroughItsJson() {
    val accounts = ServerAccounts().saved(TEAM).saved(OTHER)
    assertEquals(accounts, ServerAccounts.decode(accounts.encode()))
  }

  @Test
  fun unreadableDataReadsAsNoAccounts() {
    assertEquals(ServerAccounts(), ServerAccounts.decode(null))
    assertEquals(ServerAccounts(), ServerAccounts.decode("not json"))
    assertEquals(ServerAccounts(), ServerAccounts.decode("{\"accounts\":42}"))
    assertNull(ServerAccounts.decode("{\"accounts\":[],\"active_origin\":\"https://team.example.com\"}").maybeActive)
  }
}

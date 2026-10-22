package org.teamcraft.android

import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import kotlinx.coroutines.runBlocking
import org.junit.After
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.teamcraft.android.security.KeystoreBox
import org.teamcraft.android.security.SealedFile
import uniffi.teamcraft_ffi.AppConfig
import uniffi.teamcraft_ffi.EventListener
import uniffi.teamcraft_ffi.TeamCraft
import uniffi.teamcraft_ffi.TeamCraftException
import uniffi.teamcraft_ffi.TokenProvider

/**
 * The Rust core on a real device: the library loads through JNA, the engine thread starts, the
 * sealed SQLite store opens, and errors come back as Kotlin exceptions. Also the Keystore sealing.
 */
@RunWith(AndroidJUnit4::class)
class CoreOnDeviceTest {
  private val context = InstrumentationRegistry.getInstrumentation().targetContext
  private lateinit var directory: File

  @Before
  fun createDirectory() {
    directory = File(context.cacheDir, "core-test-${System.nanoTime()}").apply { mkdirs() }
  }

  @After
  fun removeDirectory() {
    directory.deleteRecursively()
  }

  @Test
  fun coreStartsAndReportsAnUnreachableServer() = runBlocking {
    val core = TeamCraft(config(), Tokens("token"), Listener)
    try {
      core.accountState()
      fail("an unreachable server must fail")
    } catch (expected: TeamCraftException.Network) {
      // The request went out through the Rust HTTP stack and failed as a network error.
    }
    try {
      core.conversations()
      fail("not enrolled yet")
    } catch (expected: TeamCraftException.NotEnrolled) {
    }
    val code = core.startDeviceLink()
    assertTrue(code.startsWith("tclink1:"))
    core.close()
  }

  @Test
  fun signedOutIsReportedWithoutAToken() = runBlocking {
    val core = TeamCraft(config(), Tokens(null), Listener)
    try {
      core.accountState()
      fail("no token")
    } catch (expected: TeamCraftException.SignedOut) {
    }
    core.close()
  }

  @Test
  fun keystoreSealsAndOpensSecrets() {
    val file = SealedFile(File(directory, "secret.bin"), KeystoreBox("teamcraft_test"))
    val first = file.readOrCreate(32)
    assertArrayEquals(first, file.readOrCreate(32))
    assertTrue(File(directory, "secret.bin").readBytes().size > 32)
  }

  private fun config() = AppConfig(
    serverUrl = "http://127.0.0.1:9",
    dataDir = directory.absolutePath,
    account = "device-test",
    databaseKey = ByteArray(32) { it.toByte() },
  )

  private class Tokens(private val maybeToken: String?) : TokenProvider {
    override fun accessToken(): String? = maybeToken
  }

  private object Listener : EventListener {
    override fun onChange() {}
  }
}

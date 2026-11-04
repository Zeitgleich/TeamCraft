package de.zeitgleich.teamcraft.core

import android.content.Context
import java.io.File
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import de.zeitgleich.teamcraft.BuildConfig
import de.zeitgleich.teamcraft.auth.AuthRepository
import de.zeitgleich.teamcraft.security.KeystoreBox
import de.zeitgleich.teamcraft.security.SealedFile
import de.zeitgleich.teamcraft.server.StoragePaths
import uniffi.teamcraft_ffi.AppActivity
import uniffi.teamcraft_ffi.AppConfig
import uniffi.teamcraft_ffi.ConnectionState
import uniffi.teamcraft_ffi.EventListener
import uniffi.teamcraft_ffi.TeamCraft
import uniffi.teamcraft_ffi.TokenProvider

/**
 * Owns the Rust client core ([TeamCraft]) of the signed-in account on the active server. The
 * database key is 32 random bytes sealed with a Keystore key (e2ee.md §9.2); tokens come from
 * [AuthRepository]. Store and key are kept per server and account ([StoragePaths]).
 */
class CoreRepository(
  private val context: Context,
  private val auth: AuthRepository,
  /** Runs when an account's core closes (sign-out, server or account change): drop what it decrypted into memory. */
  private val onAccountClosed: () -> Unit = {},
) {
  private val lock = Mutex()

  @Volatile private var maybeCore: TeamCraft? = null
  private var maybeAccount: String? = null
  private val changes = MutableStateFlow(0L)
  private val connection = MutableStateFlow<ConnectionState?>(null)

  /** Increments whenever the local store changed; screens re-read on every value. */
  val changeCount: StateFlow<Long> = changes

  /** Where the core's gateway connection stands (`null`: no core open); the banner follows it. */
  val connectionState: StateFlow<ConnectionState?> = connection

  /** The network changed: the open core checks its connection now. Opens no core. */
  fun networkChanged() {
    maybeCore?.networkChanged()
  }

  /**
   * What the user does with the app, for presence (issue #472): the open core tells the server,
   * input at most every 30 s. Opens no core and never waits, so it is fine on every touch.
   */
  fun reportActivity(activity: AppActivity) {
    // A core closed meanwhile (sign-out, another account) has nothing to report to.
    runCatching { maybeCore?.reportActivity(activity) }
  }

  /** The core of the signed-in account, opened on first use (and again after a server change). */
  suspend fun core(): TeamCraft = lock.withLock {
    val server = auth.server ?: throw NotSignedInException("no server")
    val subject = auth.account ?: throw NotSignedInException("not signed in")
    val storageKey = server.address.storageKey
    val account = "${server.address.origin} $subject"
    maybeCore?.takeIf { maybeAccount == account }?.let { return it }
    maybeTokens?.maybeCore = null
    maybeTokens = null
    maybeCore?.let {
      it.close()
      onAccountClosed()
    }
    val tokens = Tokens(auth)
    val core = withContext(Dispatchers.IO) {
      TeamCraft(
        AppConfig(
          serverUrl = server.address.origin,
          dataDir = File(context.filesDir, StoragePaths.storeDir(storageKey)).absolutePath,
          account = StoragePaths.storeName(subject),
          databaseKey = databaseKey(storageKey, subject),
          pushGatewayUrl = BuildConfig.PUSH_GATEWAY_URL.ifEmpty { null },
        ),
        tokens,
        Changes {
          changes.update { it + 1 }
          connection.value = maybeCore?.connectionState()
        },
      )
    }
    tokens.maybeCore = core
    maybeTokens = tokens
    core.noteIdToken(auth.idToken())
    maybeCore = core
    maybeAccount = account
    connection.value = core.connectionState()
    core
  }

  /** Closes the core; local data stays (it belongs to the account, not the session). */
  suspend fun close() = lock.withLock {
    maybeTokens?.maybeCore = null
    maybeTokens = null
    maybeCore?.close()
    onAccountClosed()
    maybeCore = null
    maybeAccount = null
    connection.value = null
  }

  /** The token source of the open core, so it stops talking to that core when it closes. */
  private var maybeTokens: Tokens? = null

  private fun databaseKey(storageKey: String, subject: String): ByteArray =
    SealedFile(File(context.filesDir, StoragePaths.databaseKeyFile(storageKey, subject)), KeystoreBox(DATABASE_KEY_ALIAS))
      .readOrCreate(DATABASE_KEY_LENGTH)

  private class Tokens(private val auth: AuthRepository) : TokenProvider {
    /** The core this supplies, once it is open: it also gets the ID token that came along. */
    @Volatile var maybeCore: TeamCraft? = null

    /**
     * Called on the core's own token thread, never on one that reads. Every authenticated request
     * of the core passes here, so reading what the store holds doesn't wait for the device's CAs.
     * The ID token goes along for the owners' apps to verify (e2ee.md §3.1.3).
     */
    override fun accessToken(): String? = runBlocking {
      NetworkTrust.awaitTrustedCertificates()
      // A core closed in between throws: nothing to tell it then, and the token is still good.
      if (runCatching { maybeCore?.takeIdTokenRequest() }.getOrNull() == true) auth.refreshTokensSoon()
      auth.accessToken().also { runCatching { maybeCore?.noteIdToken(auth.idToken()) } }
    }
  }

  private class Changes(private val onChange: () -> Unit) : EventListener {
    override fun onChange() = onChange.invoke()
  }

  private companion object {
    const val DATABASE_KEY_ALIAS = "teamcraft_database"
    const val DATABASE_KEY_LENGTH = 32
  }
}

package de.zeitgleich.teamcraft.core

import android.content.Context
import java.io.File
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import de.zeitgleich.teamcraft.BuildConfig
import de.zeitgleich.teamcraft.auth.AuthRepository
import de.zeitgleich.teamcraft.security.KeystoreBox
import de.zeitgleich.teamcraft.security.SealedFile
import de.zeitgleich.teamcraft.server.StoragePaths
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

  /** The core of the signed-in account, opened on first use (and again after a server change). */
  suspend fun core(): TeamCraft = lock.withLock {
    val server = auth.server ?: throw IllegalStateException("no server")
    val subject = auth.account ?: throw IllegalStateException("not signed in")
    val storageKey = server.address.storageKey
    val account = "${server.address.origin} $subject"
    maybeCore?.takeIf { maybeAccount == account }?.let { return it }
    maybeCore?.let {
      it.close()
      onAccountClosed()
    }
    NetworkTrust.awaitTrustedCertificates()
    val core = withContext(Dispatchers.IO) {
      TeamCraft(
        AppConfig(
          serverUrl = server.address.origin,
          dataDir = File(context.filesDir, StoragePaths.storeDir(storageKey)).absolutePath,
          account = StoragePaths.storeName(subject),
          databaseKey = databaseKey(storageKey, subject),
          pushGatewayUrl = BuildConfig.PUSH_GATEWAY_URL.ifEmpty { null },
        ),
        Tokens(auth),
        Changes {
          changes.update { it + 1 }
          connection.value = maybeCore?.connectionState()
        },
      )
    }
    maybeCore = core
    maybeAccount = account
    connection.value = core.connectionState()
    core
  }

  /** Closes the core; local data stays (it belongs to the account, not the session). */
  suspend fun close() = lock.withLock {
    maybeCore?.close()
    onAccountClosed()
    maybeCore = null
    maybeAccount = null
    connection.value = null
  }

  private fun databaseKey(storageKey: String, subject: String): ByteArray =
    SealedFile(File(context.filesDir, StoragePaths.databaseKeyFile(storageKey, subject)), KeystoreBox(DATABASE_KEY_ALIAS))
      .readOrCreate(DATABASE_KEY_LENGTH)

  private class Tokens(private val auth: AuthRepository) : TokenProvider {
    override fun accessToken(): String? = auth.accessToken()
  }

  private class Changes(private val onChange: () -> Unit) : EventListener {
    override fun onChange() = onChange.invoke()
  }

  private companion object {
    const val DATABASE_KEY_ALIAS = "teamcraft_database"
    const val DATABASE_KEY_LENGTH = 32
  }
}

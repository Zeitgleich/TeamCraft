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
import uniffi.teamcraft_ffi.AppConfig
import uniffi.teamcraft_ffi.EventListener
import uniffi.teamcraft_ffi.TeamCraft
import uniffi.teamcraft_ffi.TokenProvider

/**
 * Owns the Rust client core ([TeamCraft]) of the signed-in account. The database key is 32 random
 * bytes sealed with a Keystore key (e2ee.md §9.2); tokens come from [AuthRepository].
 */
class CoreRepository(private val context: Context, private val auth: AuthRepository) {
  private val lock = Mutex()
  private var maybeCore: TeamCraft? = null
  private var maybeAccount: String? = null
  private val changes = MutableStateFlow(0L)

  /** Increments whenever the local store changed; screens re-read on every value. */
  val changeCount: StateFlow<Long> = changes

  /** The core of the signed-in account, opened on first use. */
  suspend fun core(): TeamCraft = lock.withLock {
    val account = auth.account ?: throw IllegalStateException("not signed in")
    maybeCore?.takeIf { maybeAccount == account }?.let { return it }
    maybeCore?.close()
    val core = withContext(Dispatchers.IO) {
      TeamCraft(
        AppConfig(
          serverUrl = BuildConfig.SERVER_URL,
          dataDir = File(context.filesDir, "store").absolutePath,
          account = storeName(account),
          databaseKey = databaseKey(account),
          pushGatewayUrl = BuildConfig.PUSH_GATEWAY_URL.ifEmpty { null },
        ),
        Tokens(auth),
        Changes { changes.update { it + 1 } },
      )
    }
    maybeCore = core
    maybeAccount = account
    core
  }

  /** Closes the core; local data stays (it belongs to the account, not the session). */
  suspend fun close() = lock.withLock {
    maybeCore?.close()
    maybeCore = null
    maybeAccount = null
  }

  private fun databaseKey(account: String): ByteArray =
    SealedFile(File(context.filesDir, "keys/${storeName(account)}.key"), KeystoreBox(DATABASE_KEY_ALIAS))
      .readOrCreate(DATABASE_KEY_LENGTH)

  private class Tokens(private val auth: AuthRepository) : TokenProvider {
    override fun accessToken(): String? = auth.accessToken()
  }

  private class Changes(private val onChange: () -> Unit) : EventListener {
    override fun onChange() = onChange.invoke()
  }

  companion object {
    private const val DATABASE_KEY_ALIAS = "teamcraft_database"
    private const val DATABASE_KEY_LENGTH = 32

    /** A file-name-safe name for an OIDC subject (UUIDs pass unchanged). */
    fun storeName(subject: String): String =
      subject.map { if (it.isLetterOrDigit() || it == '-' || it == '_') it else '_' }.joinToString("")
  }
}

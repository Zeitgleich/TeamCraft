package de.zeitgleich.teamcraft.server

import android.content.Context
import androidx.core.content.edit
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.SerializationException
import kotlinx.serialization.json.Json
import uniffi.teamcraft_ffi.StoredServer
import uniffi.teamcraft_ffi.storedServer
import uniffi.teamcraft_ffi.storedServerWithLoginHint

private const val PREFS = "servers"
private const val KEY_ACCOUNTS = "accounts"

private val json = Json { ignoreUnknownKeys = true }

/**
 * The servers the user signed in to, as account entries (for several accounts later; one is
 * active today). Only what the user typed or the managed configuration named ever gets here: the
 * app never switches servers because a server said so. Kept in app-private preferences, which are
 * excluded from cloud backup and device transfer (`backup_rules.xml`, `data_extraction_rules.xml`).
 */
class ServerStore(private val context: Context) {
  private val prefs = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

  /**
   * The active account's server, checked again by the core. A profile that no longer passes
   * (tampered, or from an incompatible version) is forgotten, and the user picks the server again.
   */
  @Synchronized
  fun active(): StoredServer? {
    val accounts = accounts()
    val account = accounts.maybeActive ?: return null
    val maybeServer = storedServer(account.profileJson)?.takeIf { it.address.origin == account.origin }
    if (maybeServer == null) write(accounts.removed(account.origin))
    return maybeServer
  }

  /**
   * Stores the server and makes it the active account's. A freshly checked profile keeps the
   * login hint of the stored one while the identity provider is the same (issue #448).
   */
  @Synchronized
  fun save(server: StoredServer) {
    val accounts = accounts()
    val maybeOlder = accounts.accounts
      .firstOrNull { it.origin == server.address.origin }
      ?.let { storedServer(it.profileJson) }
      ?.takeIf { it.issuer == server.issuer }
    val olderHint = maybeOlder?.maybeLoginHint
    val kept = if (server.maybeLoginHint == null && olderHint != null) {
      storedServerWithLoginHint(server.profileJson, olderHint) ?: server
    } else {
      server
    }
    write(accounts.saved(ServerAccount(kept.address.origin, kept.profileJson)))
  }

  /** Forgets the server's profile; the account's local data stays, keyed by the server. */
  @Synchronized
  fun forget(origin: String) {
    write(accounts().removed(origin))
  }

  private fun accounts(): ServerAccounts = ServerAccounts.decode(prefs.getString(KEY_ACCOUNTS, null))

  private fun write(accounts: ServerAccounts) {
    prefs.edit { putString(KEY_ACCOUNTS, accounts.encode()) }
  }
}

/** The stored accounts and which one is active. */
@Serializable
data class ServerAccounts(
  val accounts: List<ServerAccount> = emptyList(),
  @SerialName("active_origin") val maybeActiveOrigin: String? = null,
) {
  val maybeActive: ServerAccount?
    get() = accounts.firstOrNull { it.origin == maybeActiveOrigin }

  /** With [account] stored (replacing one of the same server) and active. */
  fun saved(account: ServerAccount): ServerAccounts =
    copy(accounts = accounts.filterNot { it.origin == account.origin } + account, maybeActiveOrigin = account.origin)

  /** Without the server at [origin]; no account is active afterwards if it was. */
  fun removed(origin: String): ServerAccounts =
    copy(accounts = accounts.filterNot { it.origin == origin }, maybeActiveOrigin = maybeActiveOrigin.takeUnless { it == origin })

  fun encode(): String = json.encodeToString(serializer(), this)

  companion object {
    /** Unreadable or missing data reads as no accounts: the user enters the server again. */
    fun decode(maybeJson: String?): ServerAccounts {
      val text = maybeJson ?: return ServerAccounts()
      return try {
        json.decodeFromString(serializer(), text)
      } catch (unreadable: SerializationException) {
        ServerAccounts()
      } catch (unreadable: IllegalArgumentException) {
        ServerAccounts()
      }
    }
  }
}

/** One account's server: its origin and the checked profile (`StoredServer.profileJson`). */
@Serializable
data class ServerAccount(
  val origin: String,
  @SerialName("profile") val profileJson: String,
)

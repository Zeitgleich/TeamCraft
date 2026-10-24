package de.zeitgleich.teamcraft.server

import android.content.Context
import java.io.File
import androidx.core.content.edit
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.SerializationException
import kotlinx.serialization.json.Json
import uniffi.teamcraft_ffi.StoredServer
import uniffi.teamcraft_ffi.storedServer

private const val PREFS = "servers"
private const val KEY_ACCOUNTS = "accounts"
private const val KEY_LAYOUT = "storage_layout"

/** When the old session was moved aside, for the bound on waiting for its server. */
private const val KEY_LEGACY_MOVED_AT = "legacy_moved_at"
/** The old session, store and keys waiting for their sign-out; file names inside it. */
const val LEGACY_DIR = "legacy"
const val LEGACY_SESSION = "session.bin"
const val LEGACY_KEYS = "keys"
const val LEGACY_STORE = "store"

/** Files keyed by server (StoragePaths); 1 was one server baked into the build. */
private const val LAYOUT_PER_SERVER = 2

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

  /** Stores the server and makes it the active account's. */
  @Synchronized
  fun save(server: StoredServer) {
    write(accounts().saved(ServerAccount(server.address.origin, server.profileJson)))
  }

  /** Forgets the server's profile; the account's local data stays, keyed by the server. */
  @Synchronized
  fun forget(origin: String) {
    write(accounts().removed(origin))
  }

  /**
   * Builds before the server entry kept one session, store and key per build-time server. They
   * are moved aside into [legacyDir], where `LegacySignOut` unregisters push and revokes the
   * refresh token before deleting them (#224); nothing else reads them again. Runs once; quick on
   * the calling thread.
   */
  @Synchronized
  fun migrateLegacyLayout() {
    if (prefs.getInt(KEY_LAYOUT, 1) >= LAYOUT_PER_SERVER) return
    val files = context.filesDir
    val legacy = File(files, LEGACY_DIR)
    legacy.mkdirs()
    File(files, "auth/session.bin").renameTo(File(legacy, LEGACY_SESSION))
    val keys = File(legacy, LEGACY_KEYS).apply { mkdirs() }
    File(files, "keys").listFiles { file -> file.isFile }?.forEach { it.renameTo(File(keys, it.name)) }
    File(files, "brand").listFiles { file -> file.isFile }?.forEach(File::delete)
    val store = File(files, "store")
    if (store.exists()) store.renameTo(File(legacy, LEGACY_STORE))
    prefs.edit {
      putLong(KEY_LEGACY_MOVED_AT, System.currentTimeMillis())
      putInt(KEY_LAYOUT, LAYOUT_PER_SERVER)
    }
  }

  /** The old session moved aside by [migrateLegacyLayout], if it is still there. */
  fun legacyDir(): File? = File(context.filesDir, LEGACY_DIR).takeIf { it.exists() }

  /** When [migrateLegacyLayout] moved the old session aside. */
  fun legacyMovedAt(): Long = prefs.getLong(KEY_LEGACY_MOVED_AT, 0L)

  /** Deletes the old session after its sign-out (slow, not on the UI thread). */
  fun removeLegacy() {
    File(context.filesDir, LEGACY_DIR).deleteRecursively()
    prefs.edit { remove(KEY_LEGACY_MOVED_AT) }
  }

  /** Removes the old store an earlier version moved aside without a sign-out (again after a process death); slow, not on the UI thread. */
  fun removeLegacyTrash() {
    legacyTrash().deleteRecursively()
  }

  private fun legacyTrash() = File(context.filesDir, "store-legacy-removed")

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

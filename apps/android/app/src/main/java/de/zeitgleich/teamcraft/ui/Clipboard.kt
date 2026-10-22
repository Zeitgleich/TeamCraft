package de.zeitgleich.teamcraft.ui

import android.content.ClipData
import android.content.ClipDescription
import android.content.ClipboardManager
import android.content.Context
import android.os.Build
import android.os.PersistableBundle
import androidx.work.CoroutineWorker
import androidx.work.ExistingWorkPolicy
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.WorkManager
import androidx.work.WorkerParameters
import androidx.work.workDataOf
import java.security.MessageDigest
import java.util.concurrent.TimeUnit

/**
 * A clipboard entry marked sensitive: Android 13+ hides it in the clipboard preview, and keyboards
 * that honour the flag keep it out of their clipboard history and sync. For decrypted messages and
 * codes.
 */
fun sensitiveClip(text: String): ClipData = ClipData.newPlainText("TeamCraft", text).apply {
  description.extras = PersistableBundle().apply {
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
      putBoolean(ClipDescription.EXTRA_IS_SENSITIVE, true)
    } else {
      // The same key before the constant existed; keyboards check it on older versions too.
      putBoolean("android.content.extra.IS_SENSITIVE", true)
    }
  }
}

/**
 * Clears a copied secret from the clipboard after [SECRET_CLIP_LIFETIME_MS], also when the screen
 * that copied it is gone or the app is in the background (WorkManager). Only a hash of the secret is
 * kept in WorkManager's database.
 */
object SecretClipboard {
  const val SECRET_CLIP_LIFETIME_MS = 60_000L
  private const val WORK_NAME = "clear-secret-clipboard"
  private const val KEY_HASH = "hash"

  fun copy(context: Context, secret: String) {
    val clipboard = context.getSystemService(ClipboardManager::class.java) ?: return
    clipboard.setPrimaryClip(sensitiveClip(secret))
    val request = OneTimeWorkRequestBuilder<ClearWorker>()
      .setInitialDelay(SECRET_CLIP_LIFETIME_MS, TimeUnit.MILLISECONDS)
      .setInputData(workDataOf(KEY_HASH to hash(secret)))
      .build()
    WorkManager.getInstance(context).enqueueUniqueWork(WORK_NAME, ExistingWorkPolicy.REPLACE, request)
  }

  private fun hash(text: String): String =
    MessageDigest.getInstance("SHA-256").digest(text.toByteArray()).joinToString("") { "%02x".format(it) }

  class ClearWorker(context: Context, parameters: WorkerParameters) : CoroutineWorker(context, parameters) {
    override suspend fun doWork(): Result {
      if (Build.VERSION.SDK_INT < Build.VERSION_CODES.P) return Result.success()
      val clipboard = applicationContext.getSystemService(ClipboardManager::class.java) ?: return Result.success()
      // From Android 10 an app in the background can't read the clipboard (null). Then it can't
      // tell whether the secret is still there and clears it anyway: the code matters more than
      // whatever was copied after it.
      val current = runCatching { clipboard.primaryClip?.takeIf { it.itemCount > 0 }?.getItemAt(0)?.text?.toString() }
        .getOrNull()
      if (current == null || hash(current) == inputData.getString(KEY_HASH)) {
        runCatching { clipboard.clearPrimaryClip() }
      }
      return Result.success()
    }
  }
}

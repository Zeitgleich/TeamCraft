package de.zeitgleich.teamcraft.ui

import android.content.ClipData
import android.content.ClipDescription
import android.os.Build
import android.os.PersistableBundle

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

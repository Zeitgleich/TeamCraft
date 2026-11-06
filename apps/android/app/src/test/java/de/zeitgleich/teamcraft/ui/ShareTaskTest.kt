package de.zeitgleich.teamcraft.ui

import java.io.File
import javax.xml.parsers.DocumentBuilderFactory
import org.junit.Assert.assertEquals
import org.junit.Test
import org.w3c.dom.Element

/**
 * The share sheet starts its target as a new document in a task of its own (NEW_DOCUMENT |
 * MULTIPLE_TASK). Each share then left another copy of the app behind, starting cold next to the
 * open one: a conversation stuck on "Loading the conversation", a Home saying "No direct messages
 * yet" (#1371). The main activity must stay in one task.
 */
class ShareTaskTest {
  @Test
  fun theMainActivityIsNeverANewDocument() {
    val activity = mainActivity()
    assertEquals("never", activity.getAttribute("android:documentLaunchMode"))
    assertEquals("singleTop", activity.getAttribute("android:launchMode"))
  }

  private fun mainActivity(): Element {
    // Unit tests run in the module's directory.
    val manifest = DocumentBuilderFactory.newInstance().newDocumentBuilder().parse(File(MANIFEST))
    val activities = manifest.getElementsByTagName("activity")
    return (0 until activities.length)
      .map { activities.item(it) as Element }
      .single { it.getAttribute("android:name") == ".ui.MainActivity" }
  }

  private companion object {
    const val MANIFEST = "src/main/AndroidManifest.xml"
  }
}

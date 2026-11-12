package de.zeitgleich.teamcraft.ui

import java.io.File
import java.nio.file.Files
import javax.xml.parsers.DocumentBuilderFactory
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import org.w3c.dom.Node

class SharedFilesTest {
  @get:Rule val folder = TemporaryFolder()

  private val filesDir get() = File(folder.root, "files")
  private val cacheDir get() = File(folder.root, "cache")

  private fun file(parent: File, path: String): File = File(parent, path).apply {
    parentFile!!.mkdirs()
    writeText("x")
  }

  private fun shareable(file: File) = SharedFiles.isShareable(filesDir, cacheDir, file)

  @Test
  fun aDownloadOfAnyServerAndAccountIsShareable() {
    assertTrue(shareable(file(filesDir, "store/chat.example.com/downloads/alice/report.pdf")))
    assertTrue(shareable(file(filesDir, "store/_/downloads/bob/folder/picture.jpg")))
  }

  @Test
  fun theDiagnosticsExportIsShareable() {
    assertTrue(shareable(file(cacheDir, "diagnostics/teamcraft-diagnostics.txt")))
  }

  @Test
  fun theOldDownloadsFolderOutsideAServerIsNotAShareRoot() {
    // What shared_files.xml named before #1761; the core never wrote there.
    assertFalse(shareable(file(filesDir, "store/downloads/old.pdf")))
  }

  @Test
  fun theSealedStoreAndOtherPrivateFilesAreNot() {
    assertFalse(shareable(file(filesDir, "store/chat.example.com/alice.teamcraft.db")))
    assertFalse(shareable(file(filesDir, "store/chat.example.com/outgoing/alice/draft.jpg")))
    assertFalse(shareable(file(filesDir, "keys/chat.example.com/alice.key")))
    assertFalse(shareable(file(filesDir, "auth/chat.example.com/session.bin")))
    assertFalse(shareable(file(cacheDir, "other/thing.txt")))
    assertFalse(shareable(File(folder.root, "elsewhere.txt")))
  }

  @Test
  fun theFoldersThemselvesAreNot() {
    File(filesDir, "store/chat.example.com/downloads/alice").mkdirs()
    assertFalse(shareable(File(filesDir, "store/chat.example.com/downloads")))
    assertFalse(shareable(File(cacheDir, "diagnostics").apply { mkdirs() }))
  }

  @Test
  fun aPathThatClimbsOutIsNot() {
    file(filesDir, "store/chat.example.com/alice.teamcraft.db")
    assertFalse(shareable(File(filesDir, "store/chat.example.com/downloads/../alice.teamcraft.db")))
    assertFalse(shareable(File(filesDir, "store/chat.example.com/downloads/alice/../../alice.teamcraft.db")))
  }

  @Test
  fun aLinkOutOfTheDownloadsIsNot() {
    val secret = file(filesDir, "store/chat.example.com/alice.teamcraft.db")
    val link = File(filesDir, "store/chat.example.com/downloads/alice/link.db")
    link.parentFile!!.mkdirs()
    Files.createSymbolicLink(link.toPath(), secret.toPath())
    assertFalse(shareable(link))
  }

  /** `shared_files.xml` and [SharedFiles.ROOTS] are one list: a root missing there crashes at runtime (#1761). */
  @Test
  fun theProvidersXmlListsExactlyTheRootsOfTheCode() {
    val xml = File("src/main/res/xml/shared_files.xml")
    assertTrue("run from the app module: ${xml.absolutePath}", xml.isFile)
    val paths = DocumentBuilderFactory.newInstance().newDocumentBuilder().parse(xml).documentElement.childNodes
    val listed = (0 until paths.length)
      .map { paths.item(it) }
      .filter { it.nodeType == Node.ELEMENT_NODE }
      .map { SharedFiles.Root(it.nodeName, it.attributes.getNamedItem("name").nodeValue, it.attributes.getNamedItem("path").nodeValue) }
    assertEquals(SharedFiles.ROOTS, listed)
  }
}

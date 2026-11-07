package ch.holzcloud.holzwave

import android.content.ClipData
import android.content.ClipDescription
import android.content.ClipboardManager
import android.content.ContentResolver
import android.net.Uri
import android.os.Build
import android.os.PersistableBundle
import android.provider.DocumentsContract
import android.provider.OpenableColumns
import java.io.File
import java.io.FileInputStream
import java.io.FileOutputStream
import java.nio.file.Files

/**
 * What the two document dialogs and the clipboard need that is not an Android lifecycle: the checks of the paths Rust hands
 * over, the bounded copies, and the clipboard's plain text rules. The plugin ([HolzwavePlugin]) shows the dialogs and answers
 * the calls; everything that touches a byte or a path is here.
 *
 * Nothing here trusts what it is given or what it receives. A path from Rust must be inside the app's private files folder
 * (the host only gives such paths; this is a second line). A document is another app's: its name is passed on as raw text and
 * never decides a path, its reported size is only a hint and the bytes are counted while they are copied, and nothing is kept of
 * its address after the call. No message of the system or of a provider ever leaves a function here: a failure is a plain
 * outcome, and the text of a clip is never logged, never put in an exception and never kept.
 */
object HolzwaveFiles {
  /** The copy block. */
  private const val BLOCK = 64 * 1024

  /** The most bytes of a clipboard text in either direction: the Binder transaction of the system clipboard is about 1 MiB. */
  const val MAX_CLIP_BYTES = 512 * 1024

  /** What a copy came to. The names are the `kind` of the answer Rust reads. */
  sealed class Copied {
    /** The bytes are copied. `name` is the provider's raw display name, `null` when it gave none. */
    class Done(val name: String?, val size: Long) : Copied()

    /** The document is above the limit; `size` is what the provider said or what the copy counted before it stopped. */
    class TooLarge(val name: String?, val size: Long?) : Copied()

    /** Anything else. No text. */
    object Failed : Copied()
  }

  // ---- the paths of Rust ---------------------------------------------------------------------------------------------------

  /**
   * The file at [path] when it is an absolute path of a regular file that is inside [filesDir] by its canonical form, none of
   * whose last component is a link. `null` otherwise. The canonical form of the folder and of the file's own folder is used, so a
   * link in the middle of the path or a `..` cannot lead out.
   */
  fun insideFiles(filesDir: File, path: String): File? {
    if (path.isEmpty() || path.length > 4096 || path.indexOf('\u0000') >= 0) return null
    val file = File(path)
    if (!file.isAbsolute) return null
    return try {
      val root = filesDir.canonicalFile
      val parent = file.parentFile?.canonicalFile ?: return null
      if (!parent.isDirectory) return null
      val canonical = File(parent, file.name)
      // The folder is canonical and the name is the last component as it was given: the file is where the path says, inside the
      // private folder, and it is not itself a link.
      if (!(canonical.path == root.path || canonical.path.startsWith(root.path + File.separator))) return null
      if (file.name == "." || file.name == ".." || file.name.isEmpty()) return null
      if (Files.isSymbolicLink(canonical.toPath())) return null
      if (!Files.isRegularFile(canonical.toPath(), java.nio.file.LinkOption.NOFOLLOW_LINKS)) return null
      canonical
    } catch (e: Exception) {
      null
    }
  }

  // ---- a document the person chose ---------------------------------------------------------------------------------------

  /** Only an address of a content provider comes out of the system's dialog; `file` and everything else is refused. */
  fun isContent(uri: Uri?): Boolean = uri != null && uri.scheme == ContentResolver.SCHEME_CONTENT

  private class Meta(val name: String?, val size: Long?)

  /** The provider's display name and size. Both may be missing, and a failure of the query is "both missing". */
  private fun meta(resolver: ContentResolver, uri: Uri): Meta {
    var name: String? = null
    var size: Long? = null
    try {
      resolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE), null, null, null)?.use { cursor ->
        if (cursor.moveToFirst()) {
          val nameAt = cursor.getColumnIndex(OpenableColumns.DISPLAY_NAME)
          if (nameAt >= 0 && !cursor.isNull(nameAt)) name = cursor.getString(nameAt)
          val sizeAt = cursor.getColumnIndex(OpenableColumns.SIZE)
          if (sizeAt >= 0 && !cursor.isNull(sizeAt)) {
            val reported = cursor.getLong(sizeAt)
            if (reported >= 0) size = reported
          }
        }
      }
    } catch (e: Exception) {
      // A provider that fails its query is a provider with no name and no size; the copy decides the rest.
    }
    return Meta(name, size)
  }

  /**
   * Copies the document at [uri] to [target], a regular empty file the host made (mode 0600, its own), at most [maxBytes]. A
   * size the provider reports above the limit is refused before a byte is read; otherwise the bytes are counted while they are
   * copied and the copy stops at the limit. A partial file is deleted on every failure. Runs on the thread it is called on:
   * the caller is an IO thread, never the main one.
   */
  fun copyIn(resolver: ContentResolver, uri: Uri, target: File, maxBytes: Long): Copied {
    val meta = meta(resolver, uri)
    val reported = meta.size
    if (reported != null && reported > maxBytes) return Copied.TooLarge(meta.name, reported)
    var written = 0L
    try {
      val input = resolver.openInputStream(uri) ?: return Copied.Failed
      input.use { source ->
        // The staging file exists and is empty: it is opened for writing and cut to nothing, which keeps its owner-only mode.
        FileOutputStream(target).use { sink ->
          val buffer = ByteArray(BLOCK)
          while (true) {
            val read = source.read(buffer)
            if (read < 0) break
            written += read
            if (written > maxBytes) {
              target.delete()
              return Copied.TooLarge(meta.name, written)
            }
            sink.write(buffer, 0, read)
          }
          sink.fd.sync()
        }
      }
    } catch (e: Exception) {
      target.delete()
      return Copied.Failed
    }
    return Copied.Done(meta.name, written)
  }

  /**
   * Copies [source], a regular file inside the private folder that the caller has just checked, to the document at [uri] that
   * the create dialog made. The document is cut to the file's bytes ("wt"). Counts what it writes. A document that was made
   * and could not be filled is removed on a best effort, so the person is not left with an empty file under the chosen name.
   */
  fun copyOut(resolver: ContentResolver, uri: Uri, source: File): Copied {
    var written = 0L
    try {
      val output = resolver.openOutputStream(uri, "wt") ?: return Copied.Failed
      output.use { sink ->
        FileInputStream(source).use { input ->
          val buffer = ByteArray(BLOCK)
          while (true) {
            val read = input.read(buffer)
            if (read < 0) break
            sink.write(buffer, 0, read)
            written += read
          }
          sink.flush()
        }
      }
    } catch (e: Exception) {
      try {
        DocumentsContract.deleteDocument(resolver, uri)
      } catch (ignored: Exception) {
        // Best effort: not every provider can delete.
      }
      return Copied.Failed
    }
    return Copied.Done(null, written)
  }

  // ---- the clipboard -------------------------------------------------------------------------------------------------------

  /** The bytes [text] has in UTF-8, counted without making a copy. A lone surrogate counts as the replacement character (3). */
  fun utf8Length(text: CharSequence): Long {
    var bytes = 0L
    var i = 0
    while (i < text.length) {
      val c = text[i]
      when {
        c.code < 0x80 -> bytes += 1
        c.code < 0x800 -> bytes += 2
        Character.isHighSurrogate(c) && i + 1 < text.length && Character.isLowSurrogate(text[i + 1]) -> {
          bytes += 4
          i++
        }
        else -> bytes += 3
      }
      i++
    }
    return bytes
  }

  /** What the clipboard's text read came to: the plain text (empty when the clip holds none) and whether it is marked sensitive. */
  class ClipText(val text: String, val sensitive: Boolean)

  /** The clip has the sensitive mark of Android 13 and later. */
  private fun marked(description: ClipDescription?): Boolean =
    Build.VERSION.SDK_INT >= 33 && description?.extras?.getBoolean(ClipDescription.EXTRA_IS_SENSITIVE, false) == true

  /**
   * The text of the primary clip, plain text only. The first item's `text` is taken (never `coerceToText`, which would turn an
   * address, an intent or a page of HTML into text), and only when the clip says it holds plain text. An empty clip, a picture
   * or a link is the empty string. Returns `null` when the text is above [limit] bytes: the caller then rejects `too_large`.
   * Nothing listens for the clipboard; this is called only when a person's own paste asked.
   */
  fun readText(clipboard: ClipboardManager, limit: Long): ClipText? {
    val clip = clipboard.primaryClip ?: return ClipText("", false)
    val description = clip.description
    if (clip.itemCount < 1 || !description.hasMimeType(ClipDescription.MIMETYPE_TEXT_PLAIN)) return ClipText("", marked(description))
    val text = clip.getItemAt(0).text ?: return ClipText("", marked(description))
    // The length in UTF-16 units is a lower bound of the bytes, so a long text is refused before it is measured or copied.
    if (text.length.toLong() > limit || utf8Length(text) > limit) return null
    return ClipText(text.toString(), marked(description))
  }

  /**
   * Replaces the primary clip with a plain text clip. The label is a fixed word that does not hold the text. From Android 13 the
   * clip is marked sensitive, so the system's confirmation does not show the text; the minimum SDK is 31, so the call is
   * guarded. Returns whether the clip that is now on the clipboard carries the mark (read back from the system). An oversized
   * clip, which the system refuses, is an exception for the caller to turn into a plain failure.
   */
  fun writeText(clipboard: ClipboardManager, text: String): Boolean {
    val clip = ClipData.newPlainText("holzWave", text)
    if (Build.VERSION.SDK_INT >= 33) {
      val extras = PersistableBundle()
      extras.putBoolean(ClipDescription.EXTRA_IS_SENSITIVE, true)
      clip.description.extras = extras
    }
    clipboard.setPrimaryClip(clip)
    return marked(clipboard.primaryClipDescription)
  }
}

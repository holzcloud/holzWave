package ch.holzcloud.holzwave

import android.Manifest
import android.app.ForegroundServiceStartNotAllowedException
import android.app.Activity
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.SystemClock
import android.util.Log
import androidx.activity.result.ActivityResult
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleOwner
import app.tauri.annotation.ActivityCallback
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSArray
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import app.tauri.plugin.PluginManager
import java.util.concurrent.atomic.AtomicBoolean
import kotlin.concurrent.thread

@InvokeArg
class ServiceCounts {
  var shells: Int = 0
  var connections: Int = 0
}

/** The protection of a new key: `auth` or `plain`. */
@InvokeArg
class GenerateArgs {
  var protection: String = ""
}

/** Bytes cross as a JSON array of numbers from 0 to 255; an array of ints keeps them unboxed so they can be zeroed. */
@InvokeArg
class PlainArgs {
  var plain: IntArray = IntArray(0)
}

@InvokeArg
class WrappedArgs {
  var wrapped: IntArray = IntArray(0)
}

@InvokeArg
class RecentsArgs {
  var hidden: Boolean = false
}

/** The open dialog: the staging file the host made inside the app's private folder, and the most bytes to copy. */
@InvokeArg
class PickInArgs {
  var staging_path: String = ""
  var max_bytes: Long = 0
}

/** The create dialog: the regular file of the shell's folder to copy out, and the name the dialog offers. */
@InvokeArg
class SaveOutArgs {
  var source_path: String = ""
  var suggested_name: String = ""
}

/** The clipboard read: the most bytes of text that may come back. */
@InvokeArg
class ClipReadArgs {
  var max_bytes: Long = 0
}

/** The clipboard write: the plain text. */
@InvokeArg
class ClipWriteArgs {
  var text: String = ""
}

/**
 * The first-party plugin of the app: the way Rust and Kotlin talk, registered from the Rust side with Tauri's plugin builder
 * (no `tauri-plugin-*` crate). The Rust side registers no invoke handler for it, so the page cannot call it; only Rust code of
 * this crate does.
 *
 * It owns the foreground service ([HolzwaveService]): Rust tells it how many shells and connections live ([serviceUpdate]),
 * it starts the service while the app is visible and the counts are not both zero, updates it while it runs, and stops it
 * when both are zero. The action `Stop all` of the notification comes back through [requestStopAll].
 *
 * Making it also sets up the platform certificate verifier ([initTls]) that both network clients of the core use, whose Kotlin
 * component is the Gradle dependency `org.rustls:rustls-platform-verifier`.
 *
 * It also carries the secret store's platform half: the commands `keystore*` create, use and remove the wrapping key in the
 * Android Keystore ([KeystoreBridge]), `elapsedRealtime` is the clock that counts the time the phone slept, and
 * `recentsHidden` hides the app's picture in the app switcher.
 *
 * And the two halves of the single-file hand-over and the clipboard: `pickIn` and `saveOut` show the system's open-document and
 * create-document dialogs and copy one file ([HolzwaveFiles]), `clipboardRead` and `clipboardWrite` read and write the system
 * clipboard as plain text. None of them is reachable from the page.
 */
@TauriPlugin
class HolzwavePlugin(private val host: Activity) : Plugin(host) {
  @Volatile private var shells = 0
  @Volatile private var connections = 0

  private val keys = KeystoreBridge(host)

  /** What Rust last asked for the app switcher picture; `null` until it asks. It belongs to the process, not to one activity. */
  @Volatile private var recentsHidden: Boolean? = null

  init {
    instance = this
    // The certificate verifier of the platform is handed the application context once, before any network code can run: this
    // class is made from the Rust side's setup, which is before the first sync run or AI request is possible. A second call
    // (a restarted activity makes the class again) finds it set up and changes nothing. A failure is never fatal and never
    // loosens a check: the Rust side keeps its readiness flag unset and every HTTPS request is refused as `tls_not_ready`.
    try {
      initTls(host.applicationContext)
    } catch (e: Throwable) {
      Log.e(TAG, "the platform certificate verifier could not be set up; HTTPS requests are refused")
    }
  }

  /** Implemented in Rust (`src/android/tls.rs`): gives the certificate verifier the application context. */
  private external fun initTls(context: Context)

  // ---- the secret store's platform half ------------------------------------------------------------------------------
  // Every failure is one fixed word (the `code` of the rejection); the message is the same word, never text the system gave.

  private fun reject(invoke: Invoke, failure: KeystoreFailure) = invoke.reject(failure.word, failure.word)

  private fun bytesOf(ints: IntArray): ByteArray? {
    val out = ByteArray(ints.size)
    for (i in ints.indices) {
      if (ints[i] < 0 || ints[i] > 255) {
        out.fill(0)
        return null
      }
      out[i] = ints[i].toByte()
    }
    return out
  }

  private fun wire(bytes: ByteArray): JSArray {
    val ints = IntArray(bytes.size) { bytes[it].toInt() and 0xff }
    val array = JSArray(ints)
    ints.fill(0)
    bytes.fill(0)
    return array
  }

  /** What the device can do: a screen lock, and whether the current key is held by the secure chip. */
  @Command
  fun keystoreCaps(invoke: Invoke) {
    try {
      val answer = JSObject()
      answer.put("has_screen_lock", keys.hasScreenLock())
      answer.put("strongbox", keys.strongBox())
      invoke.resolve(answer)
    } catch (e: Throwable) {
      invoke.reject("failed", "failed")
    }
  }

  /** A new key, `auth` (the screen lock or a biometric for every use) or `plain` (nothing), made the current one. */
  @Command
  fun keystoreGenerate(invoke: Invoke) {
    try {
      val args = invoke.parseArgs(GenerateArgs::class.java)
      if (args.protection != "auth" && args.protection != "plain") {
        invoke.reject("failed", "failed")
        return
      }
      keys.generate(args.protection == "auth")
      invoke.resolve()
    } catch (e: KeystoreFailure) {
      reject(invoke, e)
    } catch (e: Throwable) {
      invoke.reject("failed", "failed")
    }
  }

  /** Wraps the bytes with the current key. For an `auth` key the system prompt shows first. */
  @Command
  fun keystoreWrap(invoke: Invoke) {
    val plain: ByteArray?
    try {
      val args = invoke.parseArgs(PlainArgs::class.java)
      plain = bytesOf(args.plain)
      args.plain.fill(0)
    } catch (e: Throwable) {
      invoke.reject("failed", "failed")
      return
    }
    if (plain == null || plain.isEmpty() || plain.size > MAX_PLAIN) {
      plain?.fill(0)
      invoke.reject("failed", "failed")
      return
    }
    keys.wrap(plain) { wrapped, failure ->
      if (wrapped == null) {
        reject(invoke, failure ?: KeystoreFailure("failed"))
      } else {
        val answer = JSObject()
        answer.put("wrapped", wire(wrapped))
        invoke.resolve(answer)
      }
    }
  }

  /** Unwraps what `keystoreWrap` made. For an `auth` key the system prompt shows first. */
  @Command
  fun keystoreUnwrap(invoke: Invoke) {
    val wrapped: ByteArray?
    try {
      val args = invoke.parseArgs(WrappedArgs::class.java)
      wrapped = bytesOf(args.wrapped)
    } catch (e: Throwable) {
      invoke.reject("failed", "failed")
      return
    }
    if (wrapped == null || wrapped.size > MAX_WRAPPED) {
      invoke.reject("invalid", "invalid")
      return
    }
    keys.unwrap(wrapped) { plain, failure ->
      if (plain == null) {
        reject(invoke, failure ?: KeystoreFailure("failed"))
      } else {
        val answer = JSObject()
        answer.put("plain", wire(plain))
        invoke.resolve(answer)
      }
    }
  }

  /** Removes every key but the current one. */
  @Command
  fun keystoreRetire(invoke: Invoke) {
    try {
      keys.retire()
      invoke.resolve()
    } catch (e: KeystoreFailure) {
      reject(invoke, e)
    } catch (e: Throwable) {
      invoke.reject("failed", "failed")
    }
  }

  /** Removes every key. */
  @Command
  fun keystoreDelete(invoke: Invoke) {
    try {
      keys.delete()
      invoke.resolve()
    } catch (e: KeystoreFailure) {
      reject(invoke, e)
    } catch (e: Throwable) {
      invoke.reject("failed", "failed")
    }
  }

  /** Milliseconds since the device started, suspended time included (the monotonic clock stops in suspend; this one does not). */
  @Command
  fun elapsedRealtime(invoke: Invoke) {
    val answer = JSObject()
    answer.put("ms", SystemClock.elapsedRealtime())
    invoke.resolve(answer)
  }

  // ---- the app switcher picture --------------------------------------------------------------------------------------

  /**
   * Hides (`true`) or shows (`false`) the picture of the app in the app switcher. Android 13 and later have a framework call
   * that does exactly that and nothing else: it changes the picture the system keeps for the app switcher, never a screenshot
   * or a screen recording, so it needs no look at whether the app is in front. On Android 12 and 12L the call does not exist
   * and nothing hides the picture: the answer says so (`unsupported`) and nothing else is tried.
   */
  @Command
  fun recentsHidden(invoke: Invoke) {
    try {
      val args = invoke.parseArgs(RecentsArgs::class.java)
      recentsHidden = args.hidden
      host.runOnUiThread {
        applyRecents()
        invoke.resolve(recentsAnswer())
      }
    } catch (e: Throwable) {
      invoke.reject("failed", "failed")
    }
  }

  /** Main thread. Gives the activity the last value Rust asked for; the flag belongs to the activity, so a new one needs it again. */
  private fun applyRecents() {
    val hidden = recentsHidden ?: return
    if (Build.VERSION.SDK_INT >= 33) host.setRecentsScreenshotEnabled(!hidden)
  }

  private fun recentsAnswer(): JSObject {
    val supported = Build.VERSION.SDK_INT >= 33
    val answer = JSObject()
    answer.put("applied", supported && recentsHidden == true)
    answer.put("mode", if (supported) "system" else "unsupported")
    return answer
  }

  // ---- the single-file hand-over ------------------------------------------------------------------------------------------
  // Both calls wait for the person in the system's dialog, which can take minutes, so Rust makes them on a worker thread. The
  // plugin never blocks the main thread: the dialog is started on it, the result comes back on it, and the copy runs on a
  // thread of its own. One dialog at a time (the host also takes one turn); a second call is a failure, because the manager
  // keeps one result handler.

  /** A dialog is open or a copy is running. */
  private val filesBusy = AtomicBoolean(false)

  /** The answer of a hand-over. No grant to a document is kept and no folder is ever opened; the address is dropped after the call. */
  private fun answer(kind: String, name: String? = null, size: Long? = null): JSObject {
    val answer = JSObject()
    answer.put("kind", kind)
    if (name != null) answer.put("name", name)
    if (size != null) answer.put("size", size)
    return answer
  }

  private fun finishFiles(invoke: Invoke, answer: JSObject) {
    filesBusy.set(false)
    invoke.resolve(answer)
  }

  private fun failFiles(invoke: Invoke) {
    filesBusy.set(false)
    invoke.reject("failed", "failed")
  }

  private fun resolveCopied(invoke: Invoke, copied: HolzwaveFiles.Copied) {
    when (copied) {
      is HolzwaveFiles.Copied.Done -> finishFiles(invoke, answer("done", copied.name, copied.size))
      is HolzwaveFiles.Copied.TooLarge -> finishFiles(invoke, answer("too_large", copied.name, copied.size))
      HolzwaveFiles.Copied.Failed -> failFiles(invoke)
    }
  }

  /** Shows the open-document dialog and copies the one chosen document to the staging file the host made. */
  @Command
  fun pickIn(invoke: Invoke) {
    if (!filesBusy.compareAndSet(false, true)) {
      invoke.reject("failed", "failed")
      return
    }
    try {
      val args = invoke.parseArgs(PickInArgs::class.java)
      val target = HolzwaveFiles.insideFiles(host.filesDir, args.staging_path)
      // The host gives a file it just made: empty, regular, inside the private folder. Anything else is not the host's.
      if (target == null || args.max_bytes <= 0 || target.length() != 0L) {
        failFiles(invoke)
        return
      }
      val intent = Intent(Intent.ACTION_OPEN_DOCUMENT)
        .addCategory(Intent.CATEGORY_OPENABLE)
        .setType("*/*")
        .putExtra(Intent.EXTRA_ALLOW_MULTIPLE, false)
      host.runOnUiThread {
        try {
          startActivityForResult(invoke, intent, "pickInResult")
        } catch (e: Throwable) {
          failFiles(invoke)
        }
      }
    } catch (e: Throwable) {
      failFiles(invoke)
    }
  }

  /** The dialog closed. Main thread: the copy goes to a thread of its own. */
  @ActivityCallback
  fun pickInResult(invoke: Invoke, result: ActivityResult) {
    try {
      val uri = result.data?.data
      if (result.resultCode != Activity.RESULT_OK || uri == null) {
        finishFiles(invoke, answer("cancelled"))
        return
      }
      // Only an address of a content provider comes out of the system's dialog; a `file` address is not one.
      if (!HolzwaveFiles.isContent(uri)) {
        failFiles(invoke)
        return
      }
      val args = invoke.parseArgs(PickInArgs::class.java)
      // Checked again now that the person has chosen: the path must still be the host's empty file.
      val target = HolzwaveFiles.insideFiles(host.filesDir, args.staging_path)
      if (target == null || args.max_bytes <= 0 || target.length() != 0L) {
        failFiles(invoke)
        return
      }
      val resolver = host.contentResolver
      thread(name = "hw-files", isDaemon = true) {
        try {
          resolveCopied(invoke, HolzwaveFiles.copyIn(resolver, uri, target, args.max_bytes))
        } catch (e: Throwable) {
          target.delete()
          failFiles(invoke)
        }
      }
    } catch (e: Throwable) {
      failFiles(invoke)
    }
  }

  /** Shows the create-document dialog and copies a regular file of the shell's folder to the document the person names. */
  @Command
  fun saveOut(invoke: Invoke) {
    if (!filesBusy.compareAndSet(false, true)) {
      invoke.reject("failed", "failed")
      return
    }
    try {
      val args = invoke.parseArgs(SaveOutArgs::class.java)
      if (HolzwaveFiles.insideFiles(host.filesDir, args.source_path) == null) {
        failFiles(invoke)
        return
      }
      val intent = Intent(Intent.ACTION_CREATE_DOCUMENT)
        .addCategory(Intent.CATEGORY_OPENABLE)
        .setType("application/octet-stream")
        .putExtra(Intent.EXTRA_TITLE, args.suggested_name.take(255))
      host.runOnUiThread {
        try {
          startActivityForResult(invoke, intent, "saveOutResult")
        } catch (e: Throwable) {
          failFiles(invoke)
        }
      }
    } catch (e: Throwable) {
      failFiles(invoke)
    }
  }

  /** The dialog closed. Main thread: the copy goes to a thread of its own. */
  @ActivityCallback
  fun saveOutResult(invoke: Invoke, result: ActivityResult) {
    try {
      val uri = result.data?.data
      if (result.resultCode != Activity.RESULT_OK || uri == null) {
        finishFiles(invoke, answer("cancelled"))
        return
      }
      if (!HolzwaveFiles.isContent(uri)) {
        failFiles(invoke)
        return
      }
      val args = invoke.parseArgs(SaveOutArgs::class.java)
      val source = HolzwaveFiles.insideFiles(host.filesDir, args.source_path)
      if (source == null) {
        failFiles(invoke)
        return
      }
      val resolver = host.contentResolver
      thread(name = "hw-files", isDaemon = true) {
        try {
          resolveCopied(invoke, HolzwaveFiles.copyOut(resolver, uri, source))
        } catch (e: Throwable) {
          failFiles(invoke)
        }
      }
    } catch (e: Throwable) {
      failFiles(invoke)
    }
  }

  // ---- the clipboard ------------------------------------------------------------------------------------------------------
  // Plain text only, bounded in both directions, never logged. There is no listener for clipboard changes and no handler the
  // page can reach: Rust asks when a person's own paste or copy asks.

  private fun clipboard(): ClipboardManager = host.getSystemService(ClipboardManager::class.java)

  /** Reads the primary clip as plain text. `{ text, sensitive }`; a text above the bound rejects `too_large`. */
  @Command
  fun clipboardRead(invoke: Invoke) {
    try {
      val args = invoke.parseArgs(ClipReadArgs::class.java)
      if (args.max_bytes <= 0) {
        invoke.reject("failed", "failed")
        return
      }
      val read = HolzwaveFiles.readText(clipboard(), minOf(args.max_bytes, HolzwaveFiles.MAX_CLIP_BYTES.toLong()))
      if (read == null) {
        invoke.reject("too_large", "too_large")
        return
      }
      val answer = JSObject()
      answer.put("text", read.text)
      answer.put("sensitive", read.sensitive)
      invoke.resolve(answer)
    } catch (e: Throwable) {
      // Never the message: it could hold the text.
      invoke.reject("failed", "failed")
    }
  }

  /** Replaces the primary clip with a plain text clip, marked sensitive from Android 13. `{ sensitive }` says whether it carries the mark. */
  @Command
  fun clipboardWrite(invoke: Invoke) {
    try {
      val args = invoke.parseArgs(ClipWriteArgs::class.java)
      if (HolzwaveFiles.utf8Length(args.text) > HolzwaveFiles.MAX_CLIP_BYTES) {
        invoke.reject("too_large", "too_large")
        return
      }
      val marked = HolzwaveFiles.writeText(clipboard(), args.text)
      val answer = JSObject()
      answer.put("sensitive", marked)
      invoke.resolve(answer)
    } catch (e: Throwable) {
      invoke.reject("failed", "failed")
    }
  }

  /** What a debug build was asked to run at its start, once; always none in a release build (see [DebugHooks]). */
  @Command
  fun debugTake(invoke: Invoke) {
    val answer = JSObject()
    val step = DebugHooks.take()
    if (step != null) answer.put("step", step)
    invoke.resolve(answer)
  }

  /** Rust's count of the live sessions. */
  @Command
  fun serviceUpdate(invoke: Invoke) {
    val counts = invoke.parseArgs(ServiceCounts::class.java)
    shells = maxOf(counts.shells, 0)
    connections = maxOf(counts.connections, 0)
    host.runOnUiThread { apply() }
    invoke.resolve()
  }

  /** The app is in front again: a start that was refused or had to wait for it is tried now. */
  override fun onResume(activity: androidx.appcompat.app.AppCompatActivity) {
    apply()
    applyRecents()
  }

  /** The activity is going away: a prompt that is on screen is closed. */
  override fun onDestroy(activity: androidx.appcompat.app.AppCompatActivity) {
    keys.cancelPrompt()
  }

  /**
   * `Stop all` was tapped. Triggers the event Rust listens to; `false` when nobody listens (the caller then leaves the
   * notification alone, because the sessions are still there).
   */
  fun requestStopAll(): Boolean {
    if (!hasListener(STOP_ALL_EVENT)) return false
    trigger(STOP_ALL_EVENT, JSObject())
    return true
  }

  /** Whether the app has a window the person can see. Android 12 and later refuse a foreground service started without one. */
  private fun visible(): Boolean =
    (host as? LifecycleOwner)?.lifecycle?.currentState?.isAtLeast(Lifecycle.State.STARTED) == true

  private fun notificationsAllowed(): Boolean =
    Build.VERSION.SDK_INT < 33 ||
      host.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED

  /** Main thread. Brings the service in line with the last counts. */
  private fun apply() {
    val total = shells + connections
    val running = HolzwaveService.instance
    if (total == 0) {
      if (running != null) running.update(0, 0)
      return
    }
    if (running != null) {
      // Already up (started while the app was visible): only the text changes, which is allowed from the background.
      running.update(shells, connections)
      return
    }
    // Starting needs a visible app; if it is not, the counts wait here and `onResume` tries again.
    if (!visible()) return
    askForNotificationsOnce { start() }
  }

  private fun start() {
    val intent = Intent(host.applicationContext, HolzwaveService::class.java)
      .putExtra(HolzwaveService.EXTRA_SHELLS, shells)
      .putExtra(HolzwaveService.EXTRA_CONNECTIONS, connections)
    try {
      host.applicationContext.startForegroundService(intent)
    } catch (e: ForegroundServiceStartNotAllowedException) {
      // Android refused (the app was not visible after all). The counts stay; the next `onResume` tries again.
      Log.w(TAG, "the service could not start now: it is tried again when the app is in front")
    }
  }

  /**
   * On Android 13 and later the notification needs a permission the person grants once. It is asked at the first start of a
   * session, never again (whatever the answer was), and a denial does not stop anything: the service runs without a visible
   * notification and Android lists it among the active apps. `then` runs after the answer.
   */
  private fun askForNotificationsOnce(then: () -> Unit) {
    val prefs = host.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
    if (notificationsAllowed() || prefs.getBoolean(ASKED, false)) {
      then()
      return
    }
    prefs.edit().putBoolean(ASKED, true).apply()
    PluginManager.requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS)) { _ ->
      // The dialog paused the activity and the answer resumes it; start from here, which is while it is visible again.
      host.runOnUiThread { then() }
    }
  }

  companion object {
    private const val TAG = "holzwave"
    private const val STOP_ALL_EVENT = "stopAll"
    private const val PREFS = "holzwave_service"
    private const val ASKED = "notifications_asked"
    /** The data key is 32 bytes; a wrapped one is 4 + 12 + 32 + 16. The bounds are wide and refuse anything absurd. */
    private const val MAX_PLAIN = 64
    private const val MAX_WRAPPED = 256

    /** The plugin the activity made, for the service's action. Set once the Rust side registered the class. */
    @Volatile
    var instance: HolzwavePlugin? = null
  }
}

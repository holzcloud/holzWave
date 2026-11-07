package ch.holzcloud.holzwave

import android.Manifest
import android.app.ForegroundServiceStartNotAllowedException
import android.app.Activity
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.util.Log
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleOwner
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import app.tauri.plugin.PluginManager

@InvokeArg
class ServiceCounts {
  var shells: Int = 0
  var connections: Int = 0
}

/**
 * The first-party plugin of the app: the way Rust and Kotlin talk, registered from the Rust side with Tauri's plugin builder
 * (no `tauri-plugin-*` crate). The Rust side registers no invoke handler for it, so the page cannot call it; only Rust code of
 * this crate does.
 *
 * It owns the foreground service ([HolzwaveService]): Rust tells it how many shells and connections live ([serviceUpdate]),
 * it starts the service while the app is visible and the counts are not both zero, updates it while it runs, and stops it
 * when both are zero. The action `Stop all` of the notification comes back through [requestStopAll].
 */
@TauriPlugin
class HolzwavePlugin(private val host: Activity) : Plugin(host) {
  @Volatile private var shells = 0
  @Volatile private var connections = 0

  init {
    instance = this
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

    /** The plugin the activity made, for the service's action. Set once the Rust side registered the class. */
    @Volatile
    var instance: HolzwavePlugin? = null
  }
}

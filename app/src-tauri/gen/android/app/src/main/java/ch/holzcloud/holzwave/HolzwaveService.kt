package ch.holzcloud.holzwave

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Intent
import android.content.pm.ServiceInfo
import android.graphics.drawable.Icon
import android.os.Build
import android.os.IBinder
import android.util.Log

/**
 * What the notification says. Counts only: nothing that comes from a shell, a command, a host or a file ever reaches it.
 * `3 shells, 1 connection`, `1 shell`, `2 connections`.
 */
object SessionText {
  const val TITLE = "holzWave is running"
  const val CHANNEL = "Running sessions"
  const val STOP_ALL = "Stop all"

  private fun noun(count: Int, one: String, many: String): String = if (count == 1) "1 $one" else "$count $many"

  fun of(shells: Int, connections: Int): String {
    val parts = ArrayList<String>(2)
    if (shells > 0) parts.add(noun(shells, "shell", "shells"))
    if (connections > 0) parts.add(noun(connections, "connection", "connections"))
    return parts.joinToString(", ")
  }
}

/**
 * The foreground service that holds the process while shells or SSH connections live, so that Android does not freeze the
 * app a few minutes after the screen turns off. It shows one ongoing notification with the counts and one action, `Stop all`.
 * It is not exported, runs no work of its own, holds no wake lock and stops itself when both counts are zero.
 *
 * It is started only by [HolzwavePlugin], only while the app is visible, and never from a receiver or a boot event.
 */
class HolzwaveService : Service() {
  override fun onBind(intent: Intent?): IBinder? = null

  override fun onCreate() {
    super.onCreate()
    instance = this
    val channel = NotificationChannel(CHANNEL_ID, SessionText.CHANNEL, NotificationManager.IMPORTANCE_LOW)
    channel.setSound(null, null)
    channel.enableVibration(false)
    channel.enableLights(false)
    channel.setShowBadge(false)
    channel.lockscreenVisibility = Notification.VISIBILITY_PRIVATE
    getSystemService(NotificationManager::class.java).createNotificationChannel(channel)
  }

  override fun onDestroy() {
    instance = null
    super.onDestroy()
  }

  override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
    if (intent?.action == ACTION_STOP_ALL) {
      stopAll()
      return START_NOT_STICKY
    }
    val shells = intent?.getIntExtra(EXTRA_SHELLS, 0) ?: 0
    val connections = intent?.getIntExtra(EXTRA_CONNECTIONS, 0) ?: 0
    // Android wants startForeground within a few seconds of startForegroundService, whatever the counts are.
    val notification = build(shells, connections)
    if (Build.VERSION.SDK_INT >= 34) {
      startForeground(NOTIFICATION_ID, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE)
    } else {
      startForeground(NOTIFICATION_ID, notification)
    }
    if (shells + connections <= 0) end()
    return START_NOT_STICKY
  }

  /** New counts from the plugin while the service runs. Both zero ends the service and removes the notification. */
  fun update(shells: Int, connections: Int) {
    if (shells + connections <= 0) {
      end()
      return
    }
    getSystemService(NotificationManager::class.java).notify(NOTIFICATION_ID, build(shells, connections))
  }

  private fun end() {
    stopForeground(STOP_FOREGROUND_REMOVE)
    stopSelf()
  }

  /**
   * The action. The plugin tells Rust through its event, and Rust ends every shell and connection. The notification goes only
   * when somebody listens: if the Rust side is not there to hear it, the sessions still run and so does the notification.
   */
  private fun stopAll() {
    val plugin = HolzwavePlugin.instance
    if (plugin != null && plugin.requestStopAll()) {
      end()
    } else {
      Log.w(TAG, "Stop all was asked and nothing is listening")
    }
  }

  private fun build(shells: Int, connections: Int): Notification {
    val stop = PendingIntent.getService(
      this,
      0,
      Intent(this, HolzwaveService::class.java).setAction(ACTION_STOP_ALL),
      PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
    )
    val open = PendingIntent.getActivity(
      this,
      0,
      Intent(this, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP),
      PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
    )
    // On a locked screen only the title shows: the counts are readable after the unlock.
    val onLockScreen = Notification.Builder(this, CHANNEL_ID)
      .setSmallIcon(R.drawable.ic_stat_running)
      .setContentTitle(SessionText.TITLE)
      .build()
    return Notification.Builder(this, CHANNEL_ID)
      .setSmallIcon(R.drawable.ic_stat_running)
      .setContentTitle(SessionText.TITLE)
      .setContentText(SessionText.of(shells, connections))
      .setContentIntent(open)
      .setOngoing(true)
      .setOnlyAlertOnce(true)
      .setShowWhen(false)
      .setCategory(Notification.CATEGORY_SERVICE)
      .setVisibility(Notification.VISIBILITY_PRIVATE)
      .setPublicVersion(onLockScreen)
      .setForegroundServiceBehavior(Notification.FOREGROUND_SERVICE_IMMEDIATE)
      .addAction(
        Notification.Action.Builder(Icon.createWithResource(this, R.drawable.ic_stat_running), SessionText.STOP_ALL, stop).build(),
      )
      .build()
  }

  companion object {
    private const val TAG = "holzwave"
    private const val CHANNEL_ID = "running_sessions"
    private const val NOTIFICATION_ID = 1
    const val ACTION_STOP_ALL = "ch.holzcloud.holzwave.STOP_ALL"
    const val EXTRA_SHELLS = "shells"
    const val EXTRA_CONNECTIONS = "connections"

    /** The running service, for the plugin's update; `null` while it is not running. Written on the main thread only. */
    @Volatile
    var instance: HolzwaveService? = null
  }
}

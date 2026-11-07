package ch.holzcloud.holzwave

import android.content.Intent

/**
 * The release build's half of the debug hooks: nothing. The debug build has its own class of this name (in `src/debug`) that
 * lets the emulator tests start a step of the app's self-test by an intent extra. This one takes no intent, keeps nothing and
 * never answers a step, so a release APK has no trigger and no self-test.
 */
object DebugHooks {
  @Suppress("UNUSED_PARAMETER")
  fun intent(intent: Intent?) {}

  fun take(): String? = null

  fun takeArgs(): String? = null
}

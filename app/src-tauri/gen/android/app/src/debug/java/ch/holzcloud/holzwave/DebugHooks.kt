package ch.holzcloud.holzwave

import android.content.Intent

/**
 * The debug build's half of the debug hooks: an intent extra names one step of the self-test (`adb shell am start ... --es
 * holzwave_selftest <step>`), and the Rust side takes it once through the plugin and runs it. The release build has a class of
 * the same name in `src/release` that does nothing, so the name of the extra exists in debug APKs only.
 */
object DebugHooks {
  private const val EXTRA = "holzwave_selftest"

  @Volatile private var pending: String? = null

  fun intent(intent: Intent?) {
    val step = intent?.getStringExtra(EXTRA) ?: return
    // A step name is a short word of letters and dashes; anything else is ignored.
    if (step.length in 1..40 && step.all { it.isLetterOrDigit() || it == '-' }) pending = step
  }

  fun take(): String? {
    val step = pending
    pending = null
    return step
  }
}

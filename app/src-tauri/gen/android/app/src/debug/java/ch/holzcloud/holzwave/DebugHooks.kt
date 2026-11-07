package ch.holzcloud.holzwave

import android.content.Intent

/**
 * The debug build's half of the debug hooks: an intent extra names one step of the self-test (`adb shell am start ... --es
 * holzwave_selftest <step>`), and the Rust side takes it once through the plugin and runs it. The release build has a class of
 * the same name in `src/release` that does nothing, so the name of the extra exists in debug APKs only.
 */
object DebugHooks {
  private const val EXTRA = "holzwave_selftest"
  // Public values for a step (a port, a public host key, a folder name), separated by commas. The steps that need a secret read it
  // from a file the script wrote into the app's folder; nothing secret ever travels in an intent.
  private const val ARGS = "holzwave_selftest_args"
  private const val ARGS_ALLOWED = "-_./+=,:"

  @Volatile private var pending: String? = null
  @Volatile private var pendingArgs: String? = null

  fun intent(intent: Intent?) {
    val step = intent?.getStringExtra(EXTRA) ?: return
    // A step name is a short word of letters and dashes; anything else is ignored.
    if (step.length in 1..40 && step.all { it.isLetterOrDigit() || it == '-' }) {
      pending = step
      val args = intent.getStringExtra(ARGS)
      pendingArgs = args?.takeIf { it.length <= 400 && it.all { c -> c.isLetterOrDigit() || ARGS_ALLOWED.indexOf(c) >= 0 } }
    }
  }

  fun takeArgs(): String? {
    val args = pendingArgs
    pendingArgs = null
    return args
  }

  fun take(): String? {
    val step = pending
    pending = null
    return step
  }
}

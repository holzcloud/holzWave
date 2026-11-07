package ch.holzcloud.holzwave

import android.content.Intent
import android.os.Bundle
import androidx.activity.enableEdgeToEdge

class MainActivity : TauriActivity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
    // The debug build reads a self-test step from the launch intent; the release build's hook does nothing.
    DebugHooks.intent(intent)
  }

  override fun onNewIntent(intent: Intent) {
    super.onNewIntent(intent)
    DebugHooks.intent(intent)
  }
}

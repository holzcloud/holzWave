package ch.holzcloud.holzwave

import android.content.Intent
import android.os.Bundle
import android.webkit.WebView
import androidx.activity.enableEdgeToEdge

class MainActivity : TauriActivity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
    // The debug build reads a self-test step from the launch intent; the release build's hook does nothing.
    DebugHooks.intent(intent)
  }

  /** The activity's web view exists: from now on the page is told how high the system bars are (see [SystemInsets]). */
  override fun onWebViewCreate(webView: WebView) {
    super.onWebViewCreate(webView)
    SystemInsets.install(webView)
  }

  override fun onNewIntent(intent: Intent) {
    super.onNewIntent(intent)
    DebugHooks.intent(intent)
  }
}

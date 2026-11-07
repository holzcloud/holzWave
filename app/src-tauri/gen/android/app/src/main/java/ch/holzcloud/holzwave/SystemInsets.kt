package ch.holzcloud.holzwave

import android.webkit.WebView
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat
import java.lang.ref.WeakReference
import java.util.Locale
import kotlin.math.roundToInt

/**
 * The height of the system bars (the status bar, the navigation bar with its three buttons or its gesture line, a display
 * cutout), handed to the page as four CSS variables on the root element: `--hw-sys-top`, `--hw-sys-right`, `--hw-sys-bottom`,
 * `--hw-sys-left`, in CSS pixels.
 *
 * Why: the window is edge to edge, so the page is drawn under the bars and has to keep its own bars clear of them. A current
 * WebView reports that as `env(safe-area-inset-*)`; an old one does not (WebView 91 on the Android 12 image reports zero, which put
 * the bottom bar under the navigation buttons). The page takes the larger of the two (see `app/src/app.css`), so a WebView that
 * reports the insets and this value agree and one that does not is still right.
 *
 * Nothing here is reachable from the page: the page only reads the variables. The numbers are bounded and are the only thing that
 * is put into the script, so no text from outside can reach it.
 */
internal object SystemInsets {
  /** More than this is not a system bar; it also bounds what is written into the script. */
  private const val MAX_CSS_PX = 512

  private var view: WeakReference<WebView>? = null

  /** The last insets in CSS pixels as `top, right, bottom, left`; `null` until the system has reported any. */
  @Volatile
  private var latest: IntArray? = null

  /** Called on the main thread when the activity's web view exists. Every activity has its own web view, so a new one replaces the old. */
  fun install(webView: WebView) {
    view = WeakReference(webView)
    ViewCompat.setOnApplyWindowInsetsListener(webView) { v, insets ->
      val bars = insets.getInsets(WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout())
      val density = v.resources.displayMetrics.density.takeIf { it > 0f } ?: 1f
      latest = intArrayOf(css(bars.top, density), css(bars.right, density), css(bars.bottom, density), css(bars.left, density))
      push()
      // The web view still gets the insets its own way (a current WebView derives `env(safe-area-inset-*)` from them).
      ViewCompat.onApplyWindowInsets(v, insets)
    }
    ViewCompat.requestApplyInsets(webView)
  }

  private fun css(px: Int, density: Float): Int = (px / density).roundToInt().coerceIn(0, MAX_CSS_PX)

  /** The script that sets the variables: only the four bounded numbers. `null` while no insets are known. */
  fun script(values: IntArray? = latest): String? {
    val v = values ?: return null
    if (v.size != 4) return null
    val names = arrayOf("top", "right", "bottom", "left")
    val sets = names.indices.joinToString("") {
      String.format(Locale.ROOT, "s.setProperty('--hw-sys-%s','%dpx');", names[it], v[it].coerceIn(0, MAX_CSS_PX))
    }
    return "(function(){var s=document.documentElement.style;$sets})();"
  }

  /**
   * Writes the variables into the page now, if the system has reported insets and a web view exists. Called when the insets change
   * and, by the plugin command `insetsApply`, when a page has finished loading (the script is lost with the page it ran in).
   */
  fun push() {
    val webView = view?.get() ?: return
    val js = script() ?: return
    webView.post { webView.evaluateJavascript(js, null) }
  }
}

package com.worldobservationlog.wrapperlite

import android.annotation.SuppressLint
import android.content.Intent
import android.os.Bundle
import android.view.Gravity
import android.view.View
import android.webkit.WebChromeClient
import android.webkit.WebSettings
import android.webkit.WebView
import android.webkit.WebViewClient
import android.widget.FrameLayout
import android.widget.TextView
import androidx.activity.OnBackPressedCallback
import androidx.appcompat.app.AppCompatActivity
import androidx.core.content.ContextCompat
import kotlinx.coroutines.MainScope
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.launch

/** Shows wrapper's web panel; the service keeps wrapper itself running. */
class MainActivity : AppCompatActivity() {

    private val scope = MainScope()
    private lateinit var webView: WebView
    private lateinit var status: TextView
    private var loadedUrl: String? = null

    @SuppressLint("SetJavaScriptEnabled")
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        webView = WebView(this).apply {
            settings.javaScriptEnabled = true
            settings.domStorageEnabled = true
            settings.cacheMode = WebSettings.LOAD_DEFAULT
            // Plain WebChromeClient enables confirm()/alert() dialogs.
            webChromeClient = WebChromeClient()
            webViewClient = object : WebViewClient() {
                override fun onReceivedError(
                    view: WebView,
                    request: android.webkit.WebResourceRequest,
                    error: android.webkit.WebResourceError,
                ) {
                    // wrapper is (re)starting; the URL flow reloads us.
                    if (request.isForMainFrame) loadedUrl = null
                }
            }
            visibility = View.INVISIBLE
        }
        status = TextView(this).apply {
            text = getString(R.string.status_starting)
            gravity = Gravity.CENTER
            textSize = 16f
            // The window background is always dark (see styles.xml).
            setTextColor(0xFFECECF1.toInt())
            setPadding(48, 48, 48, 48)
        }
        setContentView(FrameLayout(this).apply {
            addView(webView, FrameLayout.LayoutParams.MATCH_PARENT, FrameLayout.LayoutParams.MATCH_PARENT)
            addView(status, FrameLayout.LayoutParams.MATCH_PARENT, FrameLayout.LayoutParams.MATCH_PARENT)
        })

        onBackPressedDispatcher.addCallback(this, object : OnBackPressedCallback(true) {
            override fun handleOnBackPressed() {
                if (webView.canGoBack()) webView.goBack() else moveTaskToBack(true)
            }
        })

        ContextCompat.startForegroundService(this, Intent(this, WrapperService::class.java))

        scope.launch {
            WrapperService.panelUrl.combine(WrapperService.error) { url, err -> url to err }.collect { (url, err) ->
                when {
                    url != null -> {
                        status.visibility = View.GONE
                        webView.visibility = View.VISIBLE
                        if (url != loadedUrl) {
                            loadedUrl = url
                            webView.loadUrl(url)
                        }
                    }
                    err != null -> {
                        status.text = err
                        status.visibility = View.VISIBLE
                    }
                    else -> {
                        status.text = getString(R.string.status_starting)
                        status.visibility = View.VISIBLE
                        webView.visibility = View.INVISIBLE
                    }
                }
            }
        }
    }

    override fun onDestroy() {
        scope.cancel()
        webView.destroy()
        super.onDestroy()
    }
}

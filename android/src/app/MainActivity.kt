package com.velmux.mobile

import android.os.Bundle
import android.graphics.Color
import android.graphics.drawable.ColorDrawable
import androidx.activity.ComponentActivity
import androidx.activity.SystemBarStyle
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import com.velmux.ui.app.FoundationApp

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        val session = (application as VelmuxApplication).session
        val actions = AndroidPlatformActions(this)
        setContent {
            FoundationApp(session, actions, onAppearance = { dark ->
                // Follow the actual application palette, including a manual
                // preference different from the operating system's appearance.
                // API35 forces transparent system bars; the native window
                // background must match when it is visible beyond Compose.
                window.setBackgroundDrawable(ColorDrawable(if (dark) 0xFF000000.toInt() else 0xFFF5F5F7.toInt()))
                val style = if (dark) SystemBarStyle.dark(0xFF000000.toInt())
                    else SystemBarStyle.light(Color.TRANSPARENT, Color.TRANSPARENT)
                enableEdgeToEdge(statusBarStyle = style, navigationBarStyle = style)
            })
        }
    }
}

package com.velmux.mobile

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import com.velmux.ui.app.FoundationApp

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        val session = (application as VelmuxApplication).session
        val actions = AndroidPlatformActions(this)
        setContent { FoundationApp(session, actions) }
    }
}

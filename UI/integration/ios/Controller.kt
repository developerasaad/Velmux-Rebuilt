package com.velmux.ui

import androidx.compose.ui.window.ComposeUIViewController
import com.velmux.ui.app.FoundationApp
import com.velmux.ui.app.FoundationSession

/** The native application owns the session independently of controller presentation. */
fun createController(session: FoundationSession) = ComposeUIViewController {
    FoundationApp(session)
}

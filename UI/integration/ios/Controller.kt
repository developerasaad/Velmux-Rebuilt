package com.velmux.ui

import androidx.compose.ui.window.ComposeUIViewController
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.foundation.layout.Box
import androidx.compose.ui.Modifier
import androidx.compose.ui.interop.UIKitViewController
import com.velmux.ui.app.FoundationApp
import com.velmux.ui.app.FoundationSession
import kotlinx.cinterop.ExperimentalForeignApi
import platform.UIKit.UIViewController

/** Swift owns native player resources; the shared workspace owns their presentation. */
interface NativePreviewProvider {
    fun createPreview(locator: String, onUnsupported: () -> Unit): UIViewController
    fun releasePreview(controller: UIViewController)
}

private var previewProvider: NativePreviewProvider? = null

/** Install once on the main thread before presenting shared UI. */
fun installNativePreviewProvider(provider: NativePreviewProvider) {
    previewProvider = provider
}

@OptIn(ExperimentalForeignApi::class)
@Composable
internal fun IosMediaPreview(locator: String, modifier: Modifier, onUnsupported: () -> Unit) {
    val provider = previewProvider
    val latestUnsupported = rememberUpdatedState(onUnsupported)
    if (provider == null) {
        LaunchedEffect(locator) { latestUnsupported.value() }
        Box(modifier)
        return
    }
    val controller = remember(locator, provider) {
        provider.createPreview(locator) { latestUnsupported.value() }
    }
    DisposableEffect(controller) {
        onDispose { provider.releasePreview(controller) }
    }
    UIKitViewController(factory = { controller }, modifier = modifier)
}

/** The native application owns the session independently of controller presentation. */
fun createController(session: FoundationSession) = ComposeUIViewController {
    FoundationApp(session)
}

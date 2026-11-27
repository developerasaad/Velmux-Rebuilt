package com.velmux.ui.app

import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import com.velmux.ui.IosMediaPreview

@Composable
actual fun MediaPreview(locator: String, modifier: Modifier, onUnsupported: () -> Unit) {
    IosMediaPreview(locator, modifier, onUnsupported)
}

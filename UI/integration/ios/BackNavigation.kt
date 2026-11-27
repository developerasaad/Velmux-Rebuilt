package com.velmux.ui.app

import androidx.compose.runtime.Composable

@Composable
actual fun HandleBack(enabled: Boolean, onBack: () -> Unit) { /* Navigation is exposed in the shared header. */ }

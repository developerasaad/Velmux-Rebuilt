package com.velmux.ui.app

import androidx.activity.compose.BackHandler
import androidx.compose.runtime.Composable

@Composable
actual fun HandleBack(enabled: Boolean, onBack: () -> Unit) = BackHandler(enabled, onBack)

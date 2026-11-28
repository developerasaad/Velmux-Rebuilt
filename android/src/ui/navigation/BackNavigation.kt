package com.velmux.ui.app

import androidx.activity.compose.BackHandler
import androidx.compose.runtime.Composable

@Composable
fun HandleBack(enabled: Boolean, onBack: () -> Unit) = BackHandler(enabled, onBack)

package com.velmux.ui.components

import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.unit.dp

/** Standard platform alert; callers retain request ownership and stale-error guards. */
@Composable
fun VelmuxAlert(
    title: String,
    detail: String,
    confirmLabel: String = "OK",
    onConfirm: () -> Unit,
    onDismiss: () -> Unit,
    dismissLabel: String? = null,
) {
    val textHeight = (LocalConfiguration.current.screenHeightDp.dp * 0.42f).coerceIn(96.dp, 360.dp)
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(title) },
        text = { Text(detail, Modifier.heightIn(max = textHeight).verticalScroll(rememberScrollState())) },
        confirmButton = { TextButton(onClick = onConfirm) { Text(confirmLabel) } },
        dismissButton = { dismissLabel?.let { label -> TextButton(onClick = onDismiss) { Text(label) } } },
    )
}

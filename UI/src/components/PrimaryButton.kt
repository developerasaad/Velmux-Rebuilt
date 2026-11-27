package com.velmux.ui.components

import androidx.compose.foundation.layout.RowScope
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.luminance

/** Preserve the copper action fill while using accessible foreground tokens. */
@Composable
fun PrimaryButton(
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    enabled: Boolean = true,
    content: @Composable RowScope.() -> Unit,
) {
    val palette = MaterialTheme.colorScheme
    val light = palette.background.luminance() > 0.5f
    Button(
        onClick = onClick,
        modifier = modifier,
        enabled = enabled,
        colors = ButtonDefaults.buttonColors(
            containerColor = if (light) palette.primaryContainer else palette.primary,
            contentColor = if (light) palette.onPrimaryContainer else palette.onPrimary,
        ),
        content = content,
    )
}

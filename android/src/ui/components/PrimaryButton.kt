package com.velmux.ui.components

import androidx.compose.foundation.layout.RowScope
import androidx.compose.material3.Button
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier

/** Standard Material 3 action with the neutral semantic theme. */
@Composable
fun PrimaryButton(
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    enabled: Boolean = true,
    content: @Composable RowScope.() -> Unit,
) = Button(onClick = onClick, modifier = modifier, enabled = enabled, content = content)

package com.velmux.ui.theme

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color

private val LightPalette = lightColorScheme(
    primary = Color(0xFFE8833A),
    onPrimary = Color(0xFF2A211B),
    primaryContainer = Color(0xFFF5B784),
    onPrimaryContainer = Color(0xFF2A211B),
    background = Color(0xFFFFF8F3),
    onBackground = Color(0xFF2A211B),
    surface = Color(0xFFFFF8F3),
    onSurface = Color(0xFF2A211B),
)
private val DarkPalette = darkColorScheme(
    primary = Color(0xFFF09A5A),
    onPrimary = Color(0xFF2A211B),
    primaryContainer = Color(0xFFA5531D),
    onPrimaryContainer = Color(0xFFF2E9E1),
    background = Color(0xFF17120E),
    onBackground = Color(0xFFF2E9E1),
    surface = Color(0xFF211A14),
    onSurface = Color(0xFFF2E9E1),
)

@Composable
fun VelmuxTheme(content: @Composable () -> Unit) {
    MaterialTheme(
        colorScheme = if (isSystemInDarkTheme()) DarkPalette else LightPalette,
        content = content,
    )
}

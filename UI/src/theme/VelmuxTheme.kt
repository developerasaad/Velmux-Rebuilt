package com.velmux.ui.theme

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color

private val LightPalette = lightColorScheme(
    primary = Color(0xFFA5531D),
    onPrimary = Color.White,
    primaryContainer = Color(0xFFE8833A),
    onPrimaryContainer = Color(0xFF2A211B),
    inversePrimary = Color(0xFFF5B784),
    secondary = Color(0xFF8B6348),
    onSecondary = Color.White,
    secondaryContainer = Color(0xFFF3DDCB),
    onSecondaryContainer = Color(0xFF2A211B),
    background = Color(0xFFFFF8F3),
    onBackground = Color(0xFF2A211B),
    surface = Color(0xFFFFF8F3),
    onSurface = Color(0xFF2A211B),
    onSurfaceVariant = Color(0xFF5B4D44),
    outline = Color(0xFF88756A),
    outlineVariant = Color(0xFFDCCBC0),
)
private val DarkPalette = darkColorScheme(
    primary = Color(0xFFF09A5A),
    onPrimary = Color(0xFF2A211B),
    primaryContainer = Color(0xFFA5531D),
    onPrimaryContainer = Color(0xFFF2E9E1),
    secondary = Color(0xFFE2BEA3),
    onSecondary = Color(0xFF2A211B),
    secondaryContainer = Color(0xFF493425),
    onSecondaryContainer = Color(0xFFF2E9E1),
    background = Color(0xFF17120E),
    onBackground = Color(0xFFF2E9E1),
    surface = Color(0xFF211A14),
    onSurface = Color(0xFFF2E9E1),
    onSurfaceVariant = Color(0xFFD6C4B7),
    outline = Color(0xFFA18B7C),
    outlineVariant = Color(0xFF514137),
)

@Composable
fun VelmuxTheme(appearance: String = "system", content: @Composable () -> Unit) {
    MaterialTheme(
        colorScheme = if (appearance == "dark" || (appearance == "system" && isSystemInDarkTheme())) DarkPalette else LightPalette,
        content = content,
    )
}

package com.velmux.ui.theme

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.graphics.Color

private val LightPalette = lightColorScheme(
    primary = Color(0xFF1D1D1F),
    onPrimary = Color.White,
    primaryContainer = Color(0xFFE5E5EA),
    onPrimaryContainer = Color(0xFF1D1D1F),
    inversePrimary = Color(0xFFF5F5F7),
    secondary = Color(0xFF6E6E73),
    onSecondary = Color.White,
    secondaryContainer = Color(0xFFEDEDEF),
    onSecondaryContainer = Color(0xFF1D1D1F),
    background = Color(0xFFF5F5F7),
    onBackground = Color(0xFF1D1D1F),
    surface = Color.White,
    onSurface = Color(0xFF1D1D1F),
    surfaceVariant = Color(0xFFEDEDEF),
    onSurfaceVariant = Color(0xFF6E6E73),
    outline = Color(0xFF8E8E93),
    outlineVariant = Color(0xFFD1D1D6),
    surfaceContainerLowest = Color.White,
    surfaceContainerLow = Color(0xFFF5F5F7),
    surfaceContainer = Color(0xFFF0F0F2),
    surfaceContainerHigh = Color(0xFFEDEDEF),
    surfaceContainerHighest = Color(0xFFE5E5EA),
)
private val DarkPalette = darkColorScheme(
    primary = Color(0xFFF5F5F7),
    onPrimary = Color(0xFF1C1C1E),
    primaryContainer = Color(0xFF2C2C2E),
    onPrimaryContainer = Color(0xFFF5F5F7),
    inversePrimary = Color(0xFF1D1D1F),
    secondary = Color(0xFF98989D),
    onSecondary = Color.Black,
    secondaryContainer = Color(0xFF2C2C2E),
    onSecondaryContainer = Color(0xFFF5F5F7),
    background = Color.Black,
    onBackground = Color(0xFFF5F5F7),
    surface = Color(0xFF1C1C1E),
    onSurface = Color(0xFFF5F5F7),
    surfaceVariant = Color(0xFF2C2C2E),
    onSurfaceVariant = Color(0xFF98989D),
    outline = Color(0xFF6E6E73),
    outlineVariant = Color(0xFF38383A),
    surfaceContainerLowest = Color.Black,
    surfaceContainerLow = Color(0xFF1C1C1E),
    surfaceContainer = Color(0xFF1C1C1E),
    surfaceContainerHigh = Color(0xFF2C2C2E),
    surfaceContainerHighest = Color(0xFF38383A),
)

/** Native Material 3 semantics with the Rust-owned appearance preference. */
@Composable
fun VelmuxTheme(appearance: String = "system", onAppearance: (Boolean) -> Unit = {}, content: @Composable () -> Unit) {
    val dark = appearance == "dark" || (appearance == "system" && isSystemInDarkTheme())
    LaunchedEffect(dark, onAppearance) { onAppearance(dark) }
    MaterialTheme(colorScheme = if (dark) DarkPalette else LightPalette, content = content)
}

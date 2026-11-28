package com.velmux.ui.screens

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalUriHandler
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import com.velmux.ui.app.PlatformActions
import com.velmux.ui.components.VelmuxAlert

@Suppress("UNUSED_PARAMETER")
@Composable
fun SettingsScreen(
    appearance: String,
    outputLocation: String,
    cacheBytes: Long,
    onAppearance: (String) -> Unit,
    onClearCache: () -> Unit,
    actions: PlatformActions,
    modifier: Modifier = Modifier,
) {
    val uriHandler = LocalUriHandler.current
    var confirmCache by remember { mutableStateOf(false) }
    var linkFailure by remember { mutableStateOf(false) }
    Column(
        modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(20.dp),
        verticalArrangement = Arrangement.spacedBy(16.dp),
    ) {
        Text("Appearance", Modifier.semantics { heading() }, style = MaterialTheme.typography.titleMedium)
        Column(Modifier.selectableGroup()) {
            listOf("system" to "System", "light" to "Light", "dark" to "Dark").forEach { (value, label) ->
                Row(
                    Modifier.fillMaxWidth().heightIn(min = 48.dp).selectable(
                        selected = appearance == value,
                        role = Role.RadioButton,
                        onClick = { onAppearance(value) },
                    ).padding(vertical = 6.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    RadioButton(selected = appearance == value, onClick = null)
                    Text(label, Modifier.padding(start = 12.dp))
                }
            }
        }
        Text("Output location", Modifier.semantics { heading() }, style = MaterialTheme.typography.titleMedium)
        Text(outputLocation.ifBlank { "Output location is unavailable." }, style = MaterialTheme.typography.bodyMedium)
        Text("Disposable cache", Modifier.semantics { heading() }, style = MaterialTheme.typography.titleMedium)
        Text(if (cacheBytes >= 0) formatByteCount(cacheBytes.toULong()) else "Cache usage is unavailable.")
        OutlinedButton(onClick = { confirmCache = true }, enabled = cacheBytes > 0) { Text("Clear cache") }
        Text("About", Modifier.semantics { heading() }, style = MaterialTheme.typography.titleMedium)
        Text("Velmux 0.1.0", style = MaterialTheme.typography.bodyLarge)
        Text("Offline video and audio processing.", style = MaterialTheme.typography.bodyMedium)
        TextButton(onClick = {
            try { uriHandler.openUri("https://github.com/developerasaad/Velmux-Rebuilt") }
            catch (_: Exception) { linkFailure = true }
        }) { Text("Velmux on GitHub") }
    }
    if (confirmCache) {
        VelmuxAlert(
            title = "Clear disposable cache?",
            detail = "Active work and required recovery files are protected. Original files and published results are kept.",
            confirmLabel = "Clear cache",
            onConfirm = { confirmCache = false; onClearCache() },
            onDismiss = { confirmCache = false },
            dismissLabel = "Keep cache",
        )
    }
    if (linkFailure) {
        VelmuxAlert(
            title = "Link unavailable", detail = "The GitHub page could not be opened by an installed browser.",
            onConfirm = { linkFailure = false }, onDismiss = { linkFailure = false },
        )
    }
}

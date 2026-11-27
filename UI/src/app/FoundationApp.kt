package com.velmux.ui.app

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import com.velmux.ui.theme.VelmuxTheme
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

@Composable
fun FoundationApp(session: FoundationSession) {
    var attempt by remember { mutableIntStateOf(0) }
    var report by remember { mutableStateOf<FoundationSnapshot?>(null) }
    var failure by remember { mutableStateOf<String?>(null) }
    var checking by remember { mutableStateOf(true) }

    LaunchedEffect(session, attempt) {
        checking = true
        failure = null
        try {
            report = withContext(Dispatchers.Default) { session.inspect() }
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (error: Exception) {
            report = null
            failure = error.message?.take(240) ?: "The native media engine could not be checked."
        } finally {
            checking = false
        }
    }

    VelmuxTheme {
        Surface(Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
            Column(
                modifier = Modifier.safeDrawingPadding().verticalScroll(rememberScrollState()).padding(24.dp),
                verticalArrangement = Arrangement.spacedBy(16.dp),
            ) {
                Text("Velmux", modifier = Modifier.semantics { heading() }, style = MaterialTheme.typography.headlineLarge)
                Text("Engine foundation", style = MaterialTheme.typography.titleMedium)
                Text("The native integration is available for verification. Media tools will be added in the next implementation milestones.")
                if (checking) {
                    CircularProgressIndicator(Modifier.testTag("engine-checking"))
                    Text("Checking the linked media libraries…")
                }
                report?.let { result ->
                    Text("Native media engine connected", modifier = Modifier.testTag("engine-connected"), style = MaterialTheme.typography.titleMedium)
                    Text("FFmpeg ${result.ffmpegVersion}")
                    Text("${result.encoderCount} encoders · ${result.decoderCount} decoders")
                    Text("${result.muxerCount} muxers · ${result.demuxerCount} demuxers · ${result.filterCount} filters")
                    Text("Application storage", style = MaterialTheme.typography.labelLarge)
                    Text(result.storageRoot, style = MaterialTheme.typography.bodySmall)
                }
                failure?.let { Text(it, color = MaterialTheme.colorScheme.error, modifier = Modifier.testTag("engine-error")) }
                Button(
                    onClick = { attempt += 1 },
                    enabled = !checking,
                    contentPadding = PaddingValues(horizontal = 20.dp, vertical = 12.dp),
                ) { Text("Check engine") }
            }
        }
    }
}

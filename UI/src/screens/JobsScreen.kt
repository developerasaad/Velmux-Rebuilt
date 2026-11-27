package com.velmux.ui.screens

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import com.velmux.ui.components.PrimaryButton as Button
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import com.velmux.ui.app.PlatformActions
import com.velmux.ui.app.userMessage
import com.velmux.ui.components.VelmuxAlert
import uniffi.velmux_core.JobRecord
import uniffi.velmux_core.JobState

@OptIn(ExperimentalLayoutApi::class)
@Composable
fun JobsScreen(
    jobs: List<JobRecord>,
    actions: PlatformActions,
    onCancel: (String) -> Unit,
    onRestart: (String) -> Unit,
    onRetryPublication: (String) -> Unit,
    onDismiss: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    var actionFailure by remember { mutableStateOf<String?>(null) }
    var discardId by remember { mutableStateOf<String?>(null) }
    var restartId by remember { mutableStateOf<String?>(null) }
    LazyColumn(
        modifier = modifier,
        contentPadding = PaddingValues(20.dp),
        verticalArrangement = Arrangement.spacedBy(16.dp),
    ) {
        if (jobs.isEmpty()) {
            item {
                Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text("No jobs", style = MaterialTheme.typography.titleLarge)
                    Text("Submitted conversions appear here while they run. Completed jobs are kept temporarily.")
                }
            }
        }
        items(jobs, key = { it.id }) { job ->
            Surface(
                modifier = Modifier.fillMaxWidth().testTag("job-${job.fileName}"),
                shape = MaterialTheme.shapes.large,
                color = MaterialTheme.colorScheme.surface,
                tonalElevation = 2.dp,
            ) {
                Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
                    Text(job.fileName, Modifier.semantics { heading() }, style = MaterialTheme.typography.titleMedium)
                    Text(job.sourceName, style = MaterialTheme.typography.bodyMedium)
                    Text(job.state.label(), style = MaterialTheme.typography.labelLarge, color = MaterialTheme.colorScheme.primary)
                    val active = job.state in setOf(
                        JobState.PREPARING, JobState.PROCESSING, JobState.FINALIZING,
                        JobState.VALIDATING, JobState.PUBLISHING, JobState.CANCELING,
                    )
                    if (active) {
                        val duration = job.durationUs
                        if (job.state == JobState.PROCESSING && duration != null && duration > 0) {
                            val progress = (job.processedUs.toDouble() / duration).coerceIn(0.0, 0.99).toFloat()
                            LinearProgressIndicator(progress = { progress }, modifier = Modifier.fillMaxWidth())
                            Text("${(progress * 100).toInt()}% processed", style = MaterialTheme.typography.bodySmall)
                        } else {
                            LinearProgressIndicator(modifier = Modifier.fillMaxWidth())
                        }
                    }
                    job.error?.let { Text(it, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.error) }
                    job.output?.let { output ->
                        Text("${output.displayName} · ${formatByteCount(output.size)}", style = MaterialTheme.typography.bodyMedium)
                        job.sourceSize?.let { sourceSize ->
                            Text("Original: ${formatByteCount(sourceSize)}", style = MaterialTheme.typography.bodySmall)
                            if (output.size > sourceSize) {
                                Text("The result is larger than the original.", style = MaterialTheme.typography.bodySmall)
                            } else if (output.size < sourceSize) {
                                Text("Saved ${formatByteCount(sourceSize - output.size)}.", style = MaterialTheme.typography.bodySmall)
                            }
                        }
                    }
                    FlowRow(
                        modifier = Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.spacedBy(8.dp),
                        verticalArrangement = Arrangement.spacedBy(4.dp),
                    ) {
                        if ((active && job.state != JobState.CANCELING) || job.state == JobState.QUEUED || job.state == JobState.PAUSED) {
                            OutlinedButton(onClick = { onCancel(job.id) }) { Text("Cancel") }
                        }
                        if (job.state == JobState.FAILED || job.state == JobState.INTERRUPTED) {
                            if (job.verified) {
                                Button(onClick = { onRetryPublication(job.id) }) { Text("Retry saving") }
                            }
                            OutlinedButton(onClick = { restartId = job.id }) { Text("Restart from beginning") }
                            TextButton(onClick = { discardId = job.id }) { Text("Discard") }
                        }
                        if (job.state == JobState.COMPLETED) {
                            job.output?.let { output ->
                                Button(onClick = {
                                    try { actions.openOutput(output.locator) { actionFailure = it } }
                                    catch (error: Exception) { actionFailure = error.userMessage("The result could not be opened.") }
                                }) { Text("Open") }
                                OutlinedButton(onClick = {
                                    try { actions.shareOutput(output.locator) { actionFailure = it } }
                                    catch (error: Exception) { actionFailure = error.userMessage("The result could not be shared.") }
                                }) { Text("Share") }
                            }
                        }
                        if (job.state == JobState.COMPLETED || job.state == JobState.CANCELED) {
                            TextButton(onClick = { onDismiss(job.id) }) { Text("Dismiss") }
                        }
                    }
                }
            }
        }
    }
    actionFailure?.let { detail ->
        VelmuxAlert(
            title = "Result unavailable", detail = detail,
            onConfirm = { actionFailure = null }, onDismiss = { actionFailure = null },
        )
    }
    discardId?.let { id ->
        VelmuxAlert("Discard this unfinished job?", "Its unpublished working output will be removed. Original media and published files are kept.", "Discard",
            onConfirm = { discardId = null; onDismiss(id) }, onDismiss = { discardId = null }, dismissLabel = "Keep job")
    }
    restartId?.let { id ->
        VelmuxAlert("Restart from the beginning?", "Unpublished work will be replaced and the accepted conversion will run again. Use Retry saving when a verified result is ready to publish.", "Restart",
            onConfirm = { restartId = null; onRestart(id) }, onDismiss = { restartId = null }, dismissLabel = "Keep job")
    }
}

private fun JobState.label(): String = name.lowercase().replaceFirstChar { it.uppercase() }

internal fun formatByteCount(bytes: ULong): String {
    val units = listOf(1_000_000_000_000UL to "TB", 1_000_000_000UL to "GB", 1_000_000UL to "MB", 1_000UL to "KB")
    val unit = units.firstOrNull { bytes >= it.first } ?: return "$bytes B"
    val whole = bytes / unit.first
    val decimal = (bytes % unit.first) * 10UL / unit.first
    return "$whole.$decimal ${unit.second}"
}

package com.velmux.ui.app

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import com.velmux.ui.components.VelmuxAlert
import com.velmux.ui.screens.JobsScreen
import com.velmux.ui.screens.SettingsScreen
import com.velmux.ui.screens.ConverterWorkspace
import com.velmux.ui.theme.VelmuxTheme
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.launch
import kotlinx.coroutines.delay
import uniffi.velmux_core.EngineException
import uniffi.velmux_core.ConversionConfig
import uniffi.velmux_core.JobRecord
import uniffi.velmux_core.JobState

private enum class Destination { Home, Workspace, Jobs, Settings }
private data class Alert(val title: String, val detail: String, val confirm: String = "OK", val cancel: String? = null, val action: (() -> Unit)? = null)

/** Navigation and transient alerts stay local; media rules and accepted jobs stay in Rust. */
@Composable
fun FoundationApp(session: FoundationSession, actions: PlatformActions? = null) {
    val media = session.media
    val scope = rememberCoroutineScope()
    var destination by remember { mutableStateOf(Destination.Home) }
    var source by remember { mutableStateOf<SourceSnapshot?>(null) }
    var config by remember { mutableStateOf<ConversionConfig?>(null) }
    var originalConfig by remember { mutableStateOf<ConversionConfig?>(null) }
    var video by remember { mutableStateOf(true) }
    var jobs by remember { mutableStateOf(emptyList<JobRecord>()) }
    var appearance by remember { mutableStateOf("system") }
    var outputLocation by remember { mutableStateOf("") }
    var cacheBytes by remember { mutableStateOf(0L) }
    var ready by remember { mutableStateOf(false) }
    var preparation by remember { mutableStateOf<MediaRequest?>(null) }
    var preparing by remember { mutableStateOf(false) }
    var submitting by remember { mutableStateOf(false) }
    var submissionKey by remember { mutableStateOf("") }
    var alert by remember { mutableStateOf<Alert?>(null) }
    var banner by remember { mutableStateOf<String?>(null) }
    var initialization by remember { mutableIntStateOf(0) }
    var selectionGeneration by remember { mutableIntStateOf(0) }

    fun showFailure(error: Throwable, title: String = "Unable to continue") {
        if (error is CancellationException || error is EngineException.Cancelled) return
        alert = Alert(title, error.message?.take(1000) ?: "The operation could not be completed. Try again.")
    }
    fun leaveWorkspace() {
        preparation?.cancel()
        selectionGeneration += 1
        val old = source
        source = null; config = null; originalConfig = null
        if (old != null) scope.launch { try { media.release(old.id) } catch (_: Exception) { /* Closed session has already released its resources. */ } }
    }
    fun back() {
        if (preparing || submitting) { selectionGeneration += 1; preparation?.cancel(); return }
        if (destination == Destination.Workspace && config != originalConfig) {
            alert = Alert("Discard these changes?", "The unsubmitted configuration will be discarded. Accepted jobs continue independently.", "Discard", "Keep editing") {
                leaveWorkspace(); destination = Destination.Home
            }
        } else {
            if (destination == Destination.Workspace) leaveWorkspace()
            banner = null; destination = Destination.Home
        }
    }
    fun jobAction(action: suspend () -> Unit) {
        scope.launch { try { action(); jobs = media.jobs() } catch (error: Exception) { showFailure(error) } }
    }

    LaunchedEffect(initialization) {
        try {
            media.ensureStarted()
            appearance = media.appearance()
            outputLocation = media.outputLocation()
            jobs = media.jobs()
            media.runtimeError()?.let { alert = Alert("Job worker unavailable", it) }
            cacheBytes = media.cacheBytes()
            ready = true
        } catch (error: Exception) { showFailure(error, "Media engine unavailable") }
    }
    LaunchedEffect(media) {
        media.jobsRevision.collectLatest {
            if (ready) {
                try { jobs = media.jobs(); media.runtimeError()?.let { alert = Alert("Job worker unavailable", it) } }
                catch (error: Exception) { showFailure(error, "Jobs unavailable") }
            }
        }
    }
    LaunchedEffect(banner) {
        val current = banner
        if(current != null) { delay(8_000); if(banner == current) banner = null }
    }
    DisposableEffect(media) {
        onDispose { selectionGeneration += 1; preparation?.cancel(); source?.let { old -> media.releaseAfterDisposal(old.id) } }
    }
    HandleBack(destination != Destination.Home || preparing) { back() }

    fun pick(isVideo: Boolean) {
        if (actions == null || preparing || submitting) return
        video = isVideo
        val generation = ++selectionGeneration
        actions.selectSource(isVideo) { selected, failure ->
            if (generation == selectionGeneration) {
                if (failure != null) alert = Alert("Source unavailable", failure)
                else if (selected != null) {
                    preparing = true
                    val request = media.beginRequest()
                    preparation = request
                    scope.launch {
                        var inspected: SourceSnapshot? = null
                        try {
                            media.ensureStarted()
                            inspected = media.inspect(selected, isVideo, request)
                            val defaults = media.defaults(inspected.id, isVideo)
                            if (generation == selectionGeneration) {
                                source = inspected; config = defaults; originalConfig = defaults
                                // A workspace owns one stable idempotency key until acceptance.
                                submissionKey = "workspace-${inspected.id}"
                                destination = Destination.Workspace
                            } else media.release(inspected.id)
                        } catch (error: Exception) {
                            inspected?.let { media.release(it.id) }
                            if (generation == selectionGeneration) showFailure(error, "Source preparation failed")
                        } finally {
                            request.close()
                            if (preparation === request) preparation = null
                            preparing = false
                        }
                    }
                }
            }
        }
    }

    VelmuxTheme(appearance) {
        Surface(Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
            Column(Modifier.fillMaxSize().safeDrawingPadding()) {
                Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    if (destination != Destination.Home) TextButton(onClick = { back() }) { Text("Back") }
                    Text(when(destination) { Destination.Home -> "Velmux"; Destination.Workspace -> if(video) "Video Converter" else "Audio Converter"; Destination.Jobs -> "Jobs"; Destination.Settings -> "Settings" }, Modifier.weight(1f).padding(vertical = 10.dp).semantics { heading() }, style = MaterialTheme.typography.titleLarge)
                    if (destination == Destination.Home) {
                        val active = jobs.count { it.state !in setOf(JobState.COMPLETED, JobState.FAILED, JobState.CANCELED) }
                        TextButton(onClick = { destination = Destination.Jobs }) { Text(if(active > 0) "Jobs ($active)" else "Jobs") }
                        TextButton(onClick = { destination = Destination.Settings; scope.launch { try { if(ready) cacheBytes = media.cacheBytes() } catch(error: Exception) { showFailure(error, "Cache unavailable") } } }) { Text("Settings") }
                    }
                }
                banner?.let { message ->
                    Surface(color = MaterialTheme.colorScheme.secondaryContainer, modifier = Modifier.fillMaxWidth().padding(16.dp)) {
                        Row(Modifier.padding(12.dp)) { Text(message, Modifier.weight(1f)); TextButton(onClick = { banner = null }) { Text("Dismiss") } }
                    }
                }
                if (preparing) {
                    Column(Modifier.fillMaxWidth().padding(24.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
                        CircularProgressIndicator()
                        Text("Inspecting the selected media…")
                        TextButton(onClick = { preparation?.cancel(); selectionGeneration += 1 }) { Text("Cancel") }
                    }
                } else when (destination) {
                    Destination.Home -> Column(Modifier.fillMaxWidth().weight(1f).verticalScroll(rememberScrollState()).padding(24.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
                        Text("Make media work for you", Modifier.fillMaxWidth(), style = MaterialTheme.typography.headlineSmall)
                        Text("Convert on your device. Your original files stay untouched.", style = MaterialTheme.typography.bodyLarge)
                        if (!ready) Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) { CircularProgressIndicator(Modifier.size(20.dp)); Text("Restoring the workspace…") }
                        Text("Video", Modifier.padding(top = 20.dp).semantics { heading() }, style = MaterialTheme.typography.titleMedium)
                        ToolRow("Video Converter", "Change format and encoding", ready) { pick(true) }
                        listOf("Crop Video", "Video Cut & Merge", "Extract Audio", "Extract Frames", "Extract Subtitles").forEach { ToolRow(it, "Not available yet", false) {} }
                        Text("Audio", Modifier.padding(top = 20.dp).semantics { heading() }, style = MaterialTheme.typography.titleMedium)
                        ToolRow("Audio Converter", "Convert audio with compatible settings", ready) { pick(false) }
                        ToolRow("Audio Cut & Merge", "Not available yet", false) {}
                        if (!ready) TextButton(onClick = { initialization += 1 }) { Text("Retry initialization") }
                    }
                    Destination.Workspace -> {
                        val selected = source; val settings = config
                        if (selected != null && settings != null) ConverterWorkspace(media, selected, settings, video, submitting, onChange = { config = it }, onConvert = {
                            if (!submitting) {
                                val acceptedConfig = settings
                                submitting = true
                                val request = media.beginRequest(); preparation = request
                                scope.launch {
                                    try {
                                        media.submit(selected.id, submissionKey, acceptedConfig, request)
                                        jobs = media.jobs(); leaveWorkspace(); destination = Destination.Jobs
                                    } catch (error: Exception) { showFailure(error, "Conversion was not accepted") }
                                    finally { request.close(); preparation = null; submitting = false }
                                }
                            }
                        }, modifier = Modifier.weight(1f))
                    }
                    Destination.Jobs -> if (!ready) LoadingSection("Restoring jobs…") else if (actions != null) JobsScreen(jobs, actions,
                        onCancel = { id -> jobAction { media.cancel(id) } }, onRestart = { id -> jobAction { media.restart(id) } },
                        onRetryPublication = { id -> jobAction { media.retryPublication(id) } }, onDismiss = { id -> jobAction { media.dismiss(id) } }, modifier = Modifier.weight(1f))
                    Destination.Settings -> if (!ready) LoadingSection("Loading settings…") else if(actions != null) SettingsScreen(appearance, outputLocation, cacheBytes,
                        onAppearance = { value -> scope.launch { try { media.setAppearance(value); appearance = value } catch(error:Exception) { showFailure(error) } } },
                        onClearCache = { scope.launch { try { media.clearCache(); cacheBytes = media.cacheBytes(); banner = "Unused preview cache cleared. Active sources, unfinished jobs and published files are protected." } catch(error:Exception) { showFailure(error) } } }, actions = actions, modifier = Modifier.weight(1f))
                }
            }
        }
        alert?.let { current ->
            VelmuxAlert(current.title, current.detail, current.confirm,
                onConfirm = { alert = null; current.action?.invoke() }, onDismiss = { alert = null }, dismissLabel = current.cancel)
        }
    }
}

@Composable
private fun LoadingSection(label: String) {
    Row(Modifier.fillMaxWidth().padding(24.dp), horizontalArrangement = Arrangement.spacedBy(12.dp)) {
        CircularProgressIndicator(Modifier.size(24.dp))
        Text(label)
    }
}

@Composable
private fun ToolRow(title: String, detail: String, enabled: Boolean, onClick: () -> Unit) {
    Column(Modifier.fillMaxWidth().clickable(enabled = enabled, onClick = onClick).padding(vertical = 14.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Text(title, style = MaterialTheme.typography.titleMedium, color = if(enabled) MaterialTheme.colorScheme.onSurface else MaterialTheme.colorScheme.onSurfaceVariant)
        Text(detail, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        HorizontalDivider(Modifier.padding(top = 12.dp))
    }
}

package com.velmux.ui.screens

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.ui.unit.dp
import com.velmux.ui.app.*
import com.velmux.ui.components.OptionSheet
import kotlinx.coroutines.CancellationException
import uniffi.velmux_core.*

@Composable
fun ConverterWorkspace(
    media: MediaSession, source: SourceSnapshot, config: ConversionConfig, video: Boolean,
    submitting: Boolean, onChange: (ConversionConfig) -> Unit, onConvert: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val formats = remember(media, source.id, video) { media.catalogForSource(source.id, video) }
    val format = formats.firstOrNull { it.format == config.format }
    var sheet by remember { mutableStateOf<String?>(null) }
    var advancedAudio by remember { mutableStateOf(false) }
    var qualityText by remember(config.quality) { mutableStateOf(config.quality.toString()) }
    val qualityValid = qualityText.toIntOrNull()?.let { it in 18..28 } ?: false
    val lossless = config.format == OutputFormat.WAV || config.format == OutputFormat.FLAC
    val lossyControls = !lossless && (video.not() || config.audioCodec == AudioCodec.AAC)

    Column(modifier.fillMaxSize()) {
        Column(Modifier.weight(1f).verticalScroll(rememberScrollState()).padding(horizontal = 24.dp), verticalArrangement = Arrangement.spacedBy(14.dp)) {
            Text(source.name, style = MaterialTheme.typography.titleMedium)
            Text(source.summary, style = MaterialTheme.typography.bodySmall)
            Text(listOfNotNull(source.durationUs?.let { duration(it) }, source.byteSize?.let { bytes(it) }).joinToString(" · "))
            WorkspacePreview(media, source, video)
            Choice("Format", format?.label ?: config.format.name) { sheet = "Format" }
            if (video) {
                Choice("Codec", videoCodec(config.videoCodec)) { sheet = "Codec" }
                Choice("Resolution", when(config.videoSize) { VideoSize.ORIGINAL -> "Original"; VideoSize.HD720 -> "720p bounds"; VideoSize.SD480 -> "480p bounds" }) { sheet = "Resolution" }
                Choice("Frame rate", if(config.frameRate == FrameRate.ORIGINAL) "Original timing" else "30 fps") { sheet = "Frame rate" }
                if(config.videoCodec == VideoCodec.H264) {
                    Text("Encoding quality · ${config.quality}", style = MaterialTheme.typography.titleSmall)
                    Slider(value = config.quality.toFloat(), onValueChange = { onChange(config.copy(quality = it.toInt().toUByte())) }, valueRange = 18f..28f, steps = 9, enabled = !submitting)
                    OutlinedTextField(qualityText, onValueChange = { value ->
                        qualityText = value
                        value.toIntOrNull()?.takeIf { it in 18..28 }?.let { onChange(config.copy(quality = it.toUByte())) }
                    }, label = { Text("Quality value · 18–28") }, supportingText = { Text("Lower values retain more detail and usually create larger files.") },
                        isError = !qualityValid, singleLine = true, enabled = !submitting, keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number), modifier = Modifier.fillMaxWidth())
                }
                if(source.info.streams.any { it.kind == StreamKind.AUDIO }) TextButton(onClick = { advancedAudio = !advancedAudio }) { Text(if(advancedAudio) "Hide audio settings" else "Audio settings") }
            }
            if(!video || advancedAudio) {
                Choice("Audio codec", audioCodec(config.audioCodec)) { sheet = "Audio codec" }
                Choice("Sample rate", config.sampleRate?.let { "$it Hz" } ?: "Original") { sheet = "Sample rate" }
                Choice("Channels", when(config.channels) { 1u -> "Mono"; 2u -> "Stereo"; else -> "Original" }) { sheet = "Channels" }
                if(lossyControls && config.audioCodec != AudioCodec.COPY) Choice("Audio bitrate", config.audioBitrate?.let { "${it / 1000u} kbps" } ?: "Automatic") { sheet = "Audio bitrate" }
                if(lossless) Text("Lossless output uses the selected representation; no lossy bitrate applies.", style = MaterialTheme.typography.bodySmall)
            }
            if(lossless && config.audioBitrate != null || config.audioCodec == AudioCodec.COPY && (config.audioBitrate != null || config.sampleRate != null || config.channels != null)) {
                Text("The retained audio settings conflict with this choice.", color = MaterialTheme.colorScheme.error)
                TextButton(onClick = { onChange(config.copy(audioBitrate = null, sampleRate = null, channels = null)) }) { Text("Use original audio settings") }
            }
            if(config.videoCodec == VideoCodec.COPY && (config.videoSize != VideoSize.ORIGINAL || config.frameRate != FrameRate.ORIGINAL || config.quality != 23.toUByte())) {
                Text("Copying requires original video settings.", color = MaterialTheme.colorScheme.error)
                TextButton(onClick = { onChange(config.copy(videoSize = VideoSize.ORIGINAL, frameRate = FrameRate.ORIGINAL, quality = 23u)) }) { Text("Use original video settings") }
            }
            OutlinedTextField(config.fileName, onValueChange = { onChange(config.copy(fileName = it)) }, label = { Text("Output name") }, suffix = { Text(".${format?.extension ?: ""}") },
                singleLine = true, enabled = !submitting, isError = config.fileName.isBlank(), modifier = Modifier.fillMaxWidth().testTag("output-name"))
            Text("The source is checked again before acceptance. Incompatible tracks are reported instead of being discarded.", style = MaterialTheme.typography.bodySmall)
            Spacer(Modifier.height(12.dp))
        }
        Surface(shadowElevation = 3.dp) {
            Button(onClick = onConvert, enabled = !submitting && config.fileName.isNotBlank() && (config.videoCodec != VideoCodec.H264 || qualityValid), modifier = Modifier.fillMaxWidth().padding(horizontal = 24.dp, vertical = 12.dp).heightIn(min = 48.dp)) {
                if(submitting) { CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp); Spacer(Modifier.width(12.dp)) }
                Text(if(submitting) "Validating request…" else "Convert")
            }
        }
    }
    sheet?.let { title ->
        val values: List<Any> = when(title) {
            "Format" -> formats
            "Codec" -> format?.videoCodecs ?: emptyList<VideoCodec>()
            "Audio codec" -> format?.audioCodecs ?: emptyList<AudioCodec>()
            "Resolution" -> VideoSize.entries
            "Frame rate" -> FrameRate.entries
            "Sample rate" -> listOf(0u, 44100u, 48000u)
            "Channels" -> listOf(0u, 1u, 2u)
            else -> listOf(0u, 128000u, 192000u, 256000u)
        }
        val labels = values.map { value -> when(value) {
            is FormatOption -> value.label
            is VideoCodec -> videoCodec(value)
            is AudioCodec -> audioCodec(value)
            is VideoSize -> when(value) { VideoSize.ORIGINAL -> "Original"; VideoSize.HD720 -> "720p bounds"; VideoSize.SD480 -> "480p bounds" }
            is FrameRate -> if(value == FrameRate.ORIGINAL) "Original timing" else "30 fps"
            is UInt -> when(title) { "Sample rate" -> if(value == 0u) "Original" else "$value Hz"; "Channels" -> when(value) { 0u -> "Original"; 1u -> "Mono"; else -> "Stereo" }; else -> if(value == 0u) "Automatic" else "${value / 1000u} kbps" }
            else -> value.toString()
        } }
        val selected = values.indexOfFirst { value -> when(title) {
            "Format" -> (value as FormatOption).format == config.format
            "Codec" -> value == config.videoCodec
            "Audio codec" -> value == config.audioCodec
            "Resolution" -> value == config.videoSize
            "Frame rate" -> value == config.frameRate
            "Sample rate" -> value == (config.sampleRate ?: 0u)
            "Channels" -> value == (config.channels ?: 0u)
            else -> value == (config.audioBitrate ?: 0u)
        } }
        OptionSheet(title, labels, selected, onSelect = { index ->
            val value = values[index]
            onChange(when(title) {
                "Format" -> config.copy(format = (value as FormatOption).format)
                "Codec" -> config.copy(videoCodec = value as VideoCodec)
                "Audio codec" -> config.copy(audioCodec = value as AudioCodec)
                "Resolution" -> config.copy(videoSize = value as VideoSize)
                "Frame rate" -> config.copy(frameRate = value as FrameRate)
                "Sample rate" -> config.copy(sampleRate = (value as UInt).takeIf { it != 0u })
                "Channels" -> config.copy(channels = (value as UInt).takeIf { it != 0u })
                else -> config.copy(audioBitrate = (value as UInt).takeIf { it != 0u })
            }); sheet = null
        }, onDismiss = { sheet = null })
    }
}

@Composable
private fun WorkspacePreview(media: MediaSession, source: SourceSnapshot, video: Boolean) {
    var fallback by remember(source.id) { mutableStateOf(false) }
    var requestedPosition by remember(source.id) { mutableFloatStateOf(0f) }
    var seek by remember(source.id) { mutableIntStateOf(0) }
    var result by remember(source.id) { mutableStateOf<PreviewResult?>(null) }
    var error by remember(source.id) { mutableStateOf<String?>(null) }
    var loading by remember(source.id) { mutableStateOf(false) }
    LaunchedEffect(source.id, fallback, seek) {
        if(fallback) {
            result = null; error = null; loading = true
            val request = media.newRequest()
            try { result = media.preview(source.id, requestedPosition.toLong(), request) }
            catch(cancelled: CancellationException) { throw cancelled }
            catch(failure: Exception) { error = failure.message ?: "This preview could not be prepared." }
            finally { request.close(); loading = false }
        }
    }
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        if(!fallback) MediaPreview(source.locator, Modifier.fillMaxWidth().height(if(video) 220.dp else 120.dp)) { fallback = true }
        else {
            if(loading) { CircularProgressIndicator(Modifier.size(24.dp)); Text("Preparing a playable preview…") }
            result?.let { clip ->
                Text("Preview · ${duration(clip.startUs)}–${duration(clip.startUs + clip.durationUs)}", style = MaterialTheme.typography.labelLarge)
                MediaPreview(clip.locator, Modifier.fillMaxWidth().height(if(video) 220.dp else 120.dp)) { error = "The prepared preview cannot be played by this device." }
            }
            source.durationUs?.takeIf { it > 1_000_000 }?.let { length ->
                Text("Preview position · ${duration(requestedPosition.toLong())}")
                Slider(requestedPosition, onValueChange = { requestedPosition = it }, valueRange = 0f..(length - 1_000_000).toFloat(), onValueChangeFinished = { seek += 1 })
            }
            error?.let { Text(it, color = MaterialTheme.colorScheme.error); TextButton(onClick = { seek += 1 }) { Text("Retry preview") } }
        }
    }
}

@Composable
private fun Choice(title: String, value: String, onClick: () -> Unit) {
    OutlinedButton(onClick, modifier = Modifier.fillMaxWidth().heightIn(min = 52.dp).testTag("choice-$title"), contentPadding = PaddingValues(horizontal = 16.dp, vertical = 12.dp)) {
        Text(title, Modifier.weight(1f)); Text(value)
    }
}
private fun videoCodec(value: VideoCodec) = when(value) { VideoCodec.AUTOMATIC -> "Automatic"; VideoCodec.H264 -> "H.264"; VideoCodec.COPY -> "Copy compatible streams" }
private fun audioCodec(value: AudioCodec) = when(value) { AudioCodec.AUTOMATIC -> "Automatic"; AudioCodec.AAC -> "AAC"; AudioCodec.MP3 -> "MP3"; AudioCodec.PCM_S16LE -> "PCM 16-bit"; AudioCodec.FLAC -> "FLAC"; AudioCodec.COPY -> "Copy compatible streams" }
private fun duration(value: Long): String { val seconds = value.coerceAtLeast(0) / 1_000_000; return "${seconds / 60}:${(seconds % 60).toString().padStart(2, '0')}" }
private fun bytes(value: Long): String = if(value < 1_048_576) "${value / 1024} KB" else "${value / 1_048_576} MB"

package com.velmux.ui.screens

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.Dp
import com.velmux.ui.app.*
import com.velmux.ui.components.OptionSheet
import com.velmux.ui.components.PrimaryButton as Button
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
    val choices = remember(media, source.id, config.format, config.videoCodec, config.videoSize, config.frameRate, config.quality, config.audioCodec, config.sampleRate, config.channels, config.audioBitrate, config.customWidth, config.customHeight, config.customFrameRate) {
        media.configurationChoices(source.id, config)
    }
    var sheet by remember { mutableStateOf<String?>(null) }
    var advancedAudio by remember { mutableStateOf(false) }
    var qualityText by remember(config.quality) { mutableStateOf(config.quality.toString()) }
    val qualityValid = qualityText.toIntOrNull()?.let { it in 18..28 } ?: false
    var customWidthText by remember(source.id, config.videoSize, config.customWidth) { mutableStateOf(config.customWidth?.toString().orEmpty()) }
    var customHeightText by remember(source.id, config.videoSize, config.customHeight) { mutableStateOf(config.customHeight?.toString().orEmpty()) }
    var customRateNumeratorText by remember(source.id, config.frameRate, config.customFrameRate) { mutableStateOf(config.customFrameRate?.numerator?.toString().orEmpty()) }
    var customRateDenominatorText by remember(source.id, config.frameRate, config.customFrameRate) { mutableStateOf(config.customFrameRate?.denominator?.toString().orEmpty()) }
    val customWidthValid = customWidthText.toUIntOrNull()?.let { it > 0u } == true
    val customHeightValid = customHeightText.toUIntOrNull()?.let { it > 0u } == true
    val customNumeratorValid = customRateNumeratorText.toIntOrNull()?.let { it > 0 } == true
    val customDenominatorValid = customRateDenominatorText.toIntOrNull()?.let { it > 0 } == true
    val customSizeValid = config.videoSize != VideoSize.CUSTOM || customWidthValid && customHeightValid
    val customRateValid = config.frameRate != FrameRate.CUSTOM || customNumeratorValid && customDenominatorValid
    val lossless = config.format == OutputFormat.WAV || config.format == OutputFormat.FLAC
    val lossyControls = choices.audioBitrates.isNotEmpty()
    val qualityVisible = config.videoCodec == VideoCodec.H264 || config.videoCodec == VideoCodec.AUTOMATIC && config.quality != 23.toUByte()

    BoxWithConstraints(modifier.fillMaxSize()) {
        val previewHeight = (maxHeight * 0.4f).coerceIn(120.dp, if(video) 220.dp else 120.dp)
        Column(Modifier.fillMaxSize()) {
            Column(Modifier.weight(1f).verticalScroll(rememberScrollState()).padding(horizontal = 24.dp), verticalArrangement = Arrangement.spacedBy(14.dp)) {
                Text(source.name, style = MaterialTheme.typography.titleMedium)
                Text(source.summary, style = MaterialTheme.typography.bodySmall)
                Text(listOfNotNull(source.durationUs?.let { duration(it) }, source.byteSize?.let { bytes(it) }).joinToString(" · "))
                WorkspacePreview(media, source, previewHeight)
                Choice("Format", format?.label ?: config.format.name, enabled = !submitting) { sheet = "Format" }
                if (video) {
                    Choice("Codec", videoCodec(config.videoCodec), enabled = !submitting) { sheet = "Codec" }
                    Choice("Resolution", videoSize(config.videoSize), enabled = !submitting) { sheet = "Resolution" }
                    if (config.videoSize == VideoSize.CUSTOM) {
                        Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                            OutlinedTextField(customWidthText, onValueChange = { value ->
                                customWidthText = value
                                value.toUIntOrNull()?.takeIf { it > 0u }?.let { onChange(config.copy(customWidth = it)) }
                            }, label = { Text("Width bound") }, suffix = { Text("px") },
                                isError = !customWidthValid, singleLine = true, enabled = !submitting,
                                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
                                modifier = Modifier.weight(1f).testTag("custom-width"))
                            OutlinedTextField(customHeightText, onValueChange = { value ->
                                customHeightText = value
                                value.toUIntOrNull()?.takeIf { it > 0u }?.let { onChange(config.copy(customHeight = it)) }
                            }, label = { Text("Height bound") }, suffix = { Text("px") },
                                isError = !customHeightValid, singleLine = true, enabled = !submitting,
                                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
                                modifier = Modifier.weight(1f).testTag("custom-height"))
                        }
                        if (!customSizeValid) Text("Enter positive whole-number width and height bounds.", color = MaterialTheme.colorScheme.error)
                        Text("Display-oriented bounds preserve aspect ratio. Custom bounds may upscale.", style = MaterialTheme.typography.bodySmall)
                    }
                    Choice("Frame rate", frameRate(config.frameRate), enabled = !submitting) { sheet = "Frame rate" }
                    if (config.frameRate == FrameRate.CUSTOM) {
                        Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                            OutlinedTextField(customRateNumeratorText, onValueChange = { value ->
                                customRateNumeratorText = value
                                val numerator = value.toIntOrNull()
                                val denominator = customRateDenominatorText.toIntOrNull()
                                if (numerator != null && numerator > 0 && denominator != null && denominator > 0) {
                                    onChange(config.copy(customFrameRate = Rational(numerator, denominator)))
                                }
                            }, label = { Text("Numerator") },
                                isError = !customNumeratorValid, singleLine = true, enabled = !submitting,
                                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
                                modifier = Modifier.weight(1f).testTag("custom-fps-numerator"))
                            OutlinedTextField(customRateDenominatorText, onValueChange = { value ->
                                customRateDenominatorText = value
                                val numerator = customRateNumeratorText.toIntOrNull()
                                val denominator = value.toIntOrNull()
                                if (numerator != null && numerator > 0 && denominator != null && denominator > 0) {
                                    onChange(config.copy(customFrameRate = Rational(numerator, denominator)))
                                }
                            }, label = { Text("Denominator") },
                                isError = !customDenominatorValid, singleLine = true, enabled = !submitting,
                                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
                                modifier = Modifier.weight(1f).testTag("custom-fps-denominator"))
                        }
                        if (!customRateValid) Text("Enter a positive whole-number numerator and denominator.", color = MaterialTheme.colorScheme.error)
                        Text("Frame rate is numerator / denominator fps, for example 24000 / 1001.", style = MaterialTheme.typography.bodySmall)
                    }
                    if(qualityVisible) {
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
                    Choice("Audio codec", audioCodec(config.audioCodec), enabled = !submitting) { sheet = "Audio codec" }
                    Choice("Sample rate", config.sampleRate?.let { "$it Hz" } ?: "Original", enabled = !submitting) { sheet = "Sample rate" }
                    Choice("Channels", when(config.channels) { 1u -> "Mono"; 2u -> "Stereo"; else -> "Original" }, enabled = !submitting) { sheet = "Channels" }
                    if(lossyControls && config.audioCodec != AudioCodec.COPY) Choice("Audio bitrate", config.audioBitrate?.let { "${it / 1000u} kbps" } ?: "Automatic", enabled = !submitting) { sheet = "Audio bitrate" }
                    if(lossless) Text("PCM and FLAC use their selected sample representation; a lossy bitrate does not apply.", style = MaterialTheme.typography.bodySmall)
                    if((config.sampleRate ?: 0u) !in choices.sampleRates) Text("The retained sample rate is unavailable for this route. Choose an applicable sample rate.", color = MaterialTheme.colorScheme.error)
                    if((config.channels ?: 0u) !in choices.channels) Text("The retained channel count is unavailable for this route. Choose Mono or Stereo.", color = MaterialTheme.colorScheme.error)
                    if(config.audioBitrate != null && config.audioBitrate !in choices.audioBitrates && !lossless && config.audioCodec != AudioCodec.COPY) {
                        Text("The retained bitrate is unavailable at this sampling and channel count.", color = MaterialTheme.colorScheme.error)
                        TextButton(enabled = !submitting, onClick = { onChange(config.copy(audioBitrate = null)) }) { Text("Use automatic bitrate") }
                    }
                }
                if(format != null && (video && config.videoCodec !in format.videoCodecs || source.info.streams.any { it.kind == StreamKind.AUDIO } && config.audioCodec !in format.audioCodecs)) {
                    Text("The retained codec is incompatible with this format.", color = MaterialTheme.colorScheme.error)
                    TextButton(enabled = !submitting, onClick = { onChange(config.copy(videoCodec = VideoCodec.AUTOMATIC, audioCodec = AudioCodec.AUTOMATIC)) }) { Text("Use automatic codecs") }
                }
                if(video && (config.videoSize !in choices.videoSizes || config.frameRate !in choices.frameRates)) {
                    Text("This route preserves original video geometry and timing.", color = MaterialTheme.colorScheme.error)
                    TextButton(enabled = !submitting, onClick = { onChange(config.copy(videoSize = VideoSize.ORIGINAL, frameRate = FrameRate.ORIGINAL)) }) { Text("Use original geometry and timing") }
                }
                if(lossless && config.audioBitrate != null || config.audioCodec == AudioCodec.COPY && (config.audioBitrate != null || (config.sampleRate ?: 0u) !in choices.sampleRates || (config.channels ?: 0u) !in choices.channels)) {
                    Text("The retained audio settings conflict with this choice.", color = MaterialTheme.colorScheme.error)
                    TextButton(enabled = !submitting, onClick = { onChange(config.copy(audioBitrate = null, sampleRate = null, channels = null)) }) { Text("Use original audio settings") }
                }
                if(config.videoCodec == VideoCodec.COPY && (config.frameRate != FrameRate.ORIGINAL || config.quality != 23.toUByte())) {
                    Text("Copying requires original video settings.", color = MaterialTheme.colorScheme.error)
                    TextButton(enabled = !submitting, onClick = { onChange(config.copy(videoSize = VideoSize.ORIGINAL, frameRate = FrameRate.ORIGINAL, quality = 23u)) }) { Text("Use original video settings") }
                }
                OutlinedTextField(config.fileName, onValueChange = { onChange(config.copy(fileName = it)) }, label = { Text("Output name") }, suffix = { Text(".${format?.extension ?: ""}") },
                    singleLine = true, enabled = !submitting, isError = config.fileName.isBlank(), modifier = Modifier.fillMaxWidth().testTag("output-name"))
                Text("The source is checked again before acceptance. Incompatible tracks are reported instead of being discarded.", style = MaterialTheme.typography.bodySmall)
                Spacer(Modifier.height(12.dp))
            }
            Surface(shadowElevation = 3.dp) {
                Button(onClick = onConvert, enabled = !submitting && config.fileName.isNotBlank() && (!qualityVisible || qualityValid) && customSizeValid && customRateValid, modifier = Modifier.fillMaxWidth().padding(horizontal = 24.dp, vertical = 12.dp).heightIn(min = 48.dp)) {
                    if(submitting) { CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp); Spacer(Modifier.width(12.dp)) }
                    Text(if(submitting) "Validating request…" else "Convert")
                }
            }
        }
    }
    sheet?.let { title ->
        val values: List<Any> = when(title) {
            "Format" -> formats
            "Codec" -> format?.videoCodecs ?: emptyList<VideoCodec>()
            "Audio codec" -> format?.audioCodecs ?: emptyList<AudioCodec>()
            "Resolution" -> choices.videoSizes
            "Frame rate" -> choices.frameRates
            "Sample rate" -> choices.sampleRates
            "Channels" -> choices.channels
            else -> choices.audioBitrates
        }
        val labels = values.map { value -> when(value) {
            is FormatOption -> value.label
            is VideoCodec -> videoCodec(value)
            is AudioCodec -> audioCodec(value)
            is VideoSize -> videoSize(value)
            is FrameRate -> frameRate(value)
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
                "Resolution" -> if (value == VideoSize.CUSTOM) config.copy(
                    videoSize = VideoSize.CUSTOM,
                    customWidth = config.customWidth ?: choices.defaultCustomWidth,
                    customHeight = config.customHeight ?: choices.defaultCustomHeight,
                ) else config.copy(videoSize = value as VideoSize)
                "Frame rate" -> if (value == FrameRate.CUSTOM) config.copy(
                    frameRate = FrameRate.CUSTOM,
                    customFrameRate = config.customFrameRate ?: choices.defaultCustomFrameRate,
                ) else config.copy(frameRate = value as FrameRate)
                "Sample rate" -> config.copy(sampleRate = (value as UInt).takeIf { it != 0u })
                "Channels" -> config.copy(channels = (value as UInt).takeIf { it != 0u })
                else -> config.copy(audioBitrate = (value as UInt).takeIf { it != 0u })
            }); sheet = null
        }, onDismiss = { sheet = null })
    }
}

@Composable
private fun WorkspacePreview(media: MediaSession, source: SourceSnapshot, previewHeight: Dp) {
    var fallback by remember(source.id) { mutableStateOf(false) }
    var requestedPosition by remember(source.id) { mutableFloatStateOf(0f) }
    var seek by remember(source.id) { mutableIntStateOf(0) }
    var result by remember(source.id) { mutableStateOf<PreviewResult?>(null) }
    var error by remember(source.id) { mutableStateOf<String?>(null) }
    var loading by remember(source.id) { mutableStateOf(false) }
    LaunchedEffect(source.id, fallback, seek) {
        if(fallback) {
            result = null; error = null; loading = true
            val request = media.beginRequest()
            try { result = media.preview(source.id, requestedPosition.toLong(), request) }
            catch(cancelled: CancellationException) { throw cancelled }
            catch(failure: Exception) { error = failure.userMessage("This preview could not be prepared.") }
            finally { request.close(); loading = false }
        }
    }
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        if(!fallback) MediaPreview(source.locator, Modifier.fillMaxWidth().height(previewHeight)) { fallback = true }
        else {
            if(source.info.streams.any { it.video?.colorTransfer in setOf("smpte2084", "arib-std-b67") }) {
                Text("SDR compatibility preview. Output uses the accepted color settings.", style = MaterialTheme.typography.bodySmall)
            }
            if(loading) { CircularProgressIndicator(Modifier.size(24.dp)); Text("Preparing a playable preview…") }
            result?.let { clip ->
                Text("Preview · ${duration(clip.startUs)}–${duration(clip.startUs + clip.durationUs)}", style = MaterialTheme.typography.labelLarge)
                MediaPreview(clip.locator, Modifier.fillMaxWidth().height(previewHeight)) { error = "The prepared preview cannot be played by this device." }
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
private fun Choice(title: String, value: String, enabled: Boolean = true, onClick: () -> Unit) {
    OutlinedButton(onClick, enabled = enabled, modifier = Modifier.fillMaxWidth().heightIn(min = 52.dp).testTag("choice-$title"), contentPadding = PaddingValues(horizontal = 16.dp, vertical = 12.dp)) {
        Text(title, Modifier.weight(1f)); Text(value, Modifier.weight(1f), textAlign = TextAlign.End)
    }
}
private fun videoSize(value: VideoSize) = when (value) {
    VideoSize.ORIGINAL -> "Original"
    VideoSize.P2160 -> "2160p bounds"
    VideoSize.P1440 -> "1440p bounds"
    VideoSize.P1080 -> "1080p bounds"
    VideoSize.HD720 -> "720p bounds"
    VideoSize.SD480 -> "480p bounds"
    VideoSize.P360 -> "360p bounds"
    VideoSize.CUSTOM -> "Custom bounds"
}
private fun frameRate(value: FrameRate) = when (value) {
    FrameRate.ORIGINAL -> "Original timing"
    FrameRate.FPS23976 -> "23.976 fps"
    FrameRate.FPS24 -> "24 fps"
    FrameRate.FPS25 -> "25 fps"
    FrameRate.FPS2997 -> "29.97 fps"
    FrameRate.FPS30 -> "30 fps"
    FrameRate.FPS48 -> "48 fps"
    FrameRate.FPS50 -> "50 fps"
    FrameRate.FPS5994 -> "59.94 fps"
    FrameRate.FPS60 -> "60 fps"
    FrameRate.CUSTOM -> "Custom rate"
}
private fun videoCodec(value: VideoCodec) = when(value) { VideoCodec.AUTOMATIC -> "Automatic"; VideoCodec.H264 -> "H.264"; VideoCodec.COPY -> "Copy compatible streams" }
private fun audioCodec(value: AudioCodec) = when(value) { AudioCodec.AUTOMATIC -> "Automatic"; AudioCodec.AAC -> "AAC"; AudioCodec.MP3 -> "MP3"; AudioCodec.PCM_S16LE -> "PCM 16-bit"; AudioCodec.FLAC -> "FLAC"; AudioCodec.COPY -> "Copy compatible streams" }
private fun duration(value: Long): String { val seconds = value.coerceAtLeast(0) / 1_000_000; return "${seconds / 60}:${(seconds % 60).toString().padStart(2, '0')}" }
private fun bytes(value: Long): String = formatByteCount(value.toULong())

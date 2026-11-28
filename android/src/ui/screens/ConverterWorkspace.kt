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
import java.math.BigDecimal
import java.math.RoundingMode
import java.util.Locale
import uniffi.velmux_core.*

private fun scaledDecimal(text: String, precision: Int, signed: Boolean): Long? {
    if (text.length > 24 || !Regex((if (signed) "^[+-]?" else "^") + "[0-9]+(?:\\.[0-9]{1,$precision})?$").matches(text)) return null
    return try { BigDecimal(text).movePointRight(precision).setScale(0, RoundingMode.UNNECESSARY).longValueExact() }
    catch (_: ArithmeticException) { null }
    catch (_: NumberFormatException) { null }
}

@Composable
fun ConverterWorkspace(
    media: MediaSession, source: SourceSnapshot, config: ConversionConfig, video: Boolean,
    submitting: Boolean, onChange: (ConversionConfig) -> Unit, onConvert: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val formats = remember(media, source.id, video) { media.catalogForSource(source.id, video) }
    val format = formats.firstOrNull { it.format == config.format }
    val choices = remember(media, source.id, config.format, config.videoCodec, config.videoSize, config.frameRate, config.quality, config.audioCodec, config.sampleRate, config.channels, config.audioBitrate, config.customWidth, config.customHeight, config.customFrameRate, config.channelLayout, config.audioPrecision, config.compression, config.audioAdjustments) {
        media.configurationChoices(source.id, config)
    }
    var sheet by remember { mutableStateOf<String?>(null) }
    var advancedAudio by remember { mutableStateOf(false) }
    var adjustmentsExpanded by remember { mutableStateOf(false) }
    val adjustments = config.audioAdjustments ?: AudioAdjustments(0, 0uL, 0uL)
    var gainText by remember(source.id, adjustments.gainCentidb) { mutableStateOf(BigDecimal(adjustments.gainCentidb).movePointLeft(2).toPlainString()) }
    var fadeInText by remember(source.id, adjustments.fadeInUs) { mutableStateOf(BigDecimal(adjustments.fadeInUs.toString()).movePointLeft(6).toPlainString()) }
    var fadeOutText by remember(source.id, adjustments.fadeOutUs) { mutableStateOf(BigDecimal(adjustments.fadeOutUs.toString()).movePointLeft(6).toPlainString()) }
    val gain = scaledDecimal(gainText, 2, true)?.toString()?.toIntOrNull()
    val fadeIn = scaledDecimal(fadeInText, 6, false)?.takeIf { it >= 0 }?.toULong()
    val fadeOut = scaledDecimal(fadeOutText, 6, false)?.takeIf { it >= 0 }?.toULong()
    val adjustmentsValid = video || gain != null && fadeIn != null && fadeOut != null
    fun updateAdjustments(gainValue: String = gainText, inValue: String = fadeInText, outValue: String = fadeOutText) {
        val g = scaledDecimal(gainValue, 2, true)?.toString()?.toIntOrNull()
        val i = scaledDecimal(inValue, 6, false)?.takeIf { it >= 0 }?.toULong()
        val o = scaledDecimal(outValue, 6, false)?.takeIf { it >= 0 }?.toULong()
        if (g != null && i != null && o != null) onChange(config.copy(audioAdjustments = AudioAdjustments(g, i, o)))
    }
    val compression = config.compression ?: defaultCompression()
    val compressionEditor = remember(source.id) { CompressionEditorState(config.compression) }
    val compressionEdit = compressionEditor.evaluate(media, compression, choices.compressionChoices)
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
    val selectedLayout = config.channelLayout ?: when (config.channels) {
        null -> AudioLayout.ORIGINAL
        1u -> AudioLayout.MONO
        2u -> AudioLayout.STEREO
        else -> null
    }
    val selectedPrecision = config.audioPrecision ?: AudioPrecision.ORIGINAL
    LaunchedEffect(submitting) { if (submitting) sheet = null }

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
                    if(source.info.streams.any { it.kind == StreamKind.AUDIO }) TextButton(enabled = !submitting, onClick = { advancedAudio = !advancedAudio }) { Text(if(advancedAudio) "Hide audio settings" else "Audio settings") }
                }
                if(!video || advancedAudio) {
                    Choice("Audio codec", audioCodec(config.audioCodec), enabled = !submitting) { sheet = "Audio codec" }
                    Choice("Sample rate", config.sampleRate?.let { "$it Hz" } ?: "Original", enabled = !submitting) { sheet = "Sample rate" }
                    Choice("Channel layout", selectedLayout?.let(::audioLayout) ?: "Retained ${config.channels}-channel setting", enabled = !submitting && choices.channelLayouts.isNotEmpty()) { sheet = "Channel layout" }
                    if (choices.audioPrecisions.isNotEmpty()) Choice("Sample precision", audioPrecision(selectedPrecision), enabled = !submitting) { sheet = "Sample precision" }
                    if((config.sampleRate ?: 0u) !in choices.sampleRates) Text("The retained sample rate is unavailable for this route. Choose an applicable sample rate.", color = MaterialTheme.colorScheme.error)
                    if(selectedLayout == null || selectedLayout !in choices.channelLayouts) Text("The retained speaker layout is unavailable for this route. Choose an applicable layout.", color = MaterialTheme.colorScheme.error)
                    if (selectedPrecision !in choices.audioPrecisions && (choices.audioPrecisions.isNotEmpty() || selectedPrecision != AudioPrecision.ORIGINAL)) {
                        Text("The retained sample precision is unavailable for this route. Choose an applicable precision or codec.", color = MaterialTheme.colorScheme.error)
                        if (choices.audioPrecisions.isEmpty() || AudioPrecision.ORIGINAL in choices.audioPrecisions) {
                            TextButton(enabled = !submitting, onClick = { onChange(config.copy(audioPrecision = null)) }) { Text("Use original sample precision") }
                        }
                    }
                }
                CompressionControls(media, compressionEditor, compression, choices.compressionChoices, video,
                    enabled = !submitting, evaluation = compressionEdit,
                    onChange = { onChange(config.copy(compression = it, quality = 23u, audioBitrate = null)) })
                if (!video) choices.audioAdjustmentLimits?.let { limits ->
                    TextButton(enabled = !submitting, onClick = { adjustmentsExpanded = !adjustmentsExpanded }) {
                        Text(if (adjustmentsExpanded) "Hide audio adjustments" else "Audio adjustments")
                    }
                    if (adjustmentsExpanded) {
                        OutlinedTextField(gainText, onValueChange = { value -> gainText = value; updateAdjustments(gainValue = value) },
                            label = { Text("Gain") }, suffix = { Text("dB") }, singleLine = true, enabled = !submitting,
                            isError = gain == null, keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Decimal),
                            modifier = Modifier.fillMaxWidth().testTag("audio-gain"))
                        Slider(value = adjustments.gainCentidb.toFloat(), onValueChange = { value ->
                            val text = String.format(Locale.ROOT, "%.2f", value.toInt() / 100.0)
                            gainText = text; updateAdjustments(gainValue = text)
                        }, valueRange = limits.minimumGainCentidb.toFloat()..limits.maximumGainCentidb.toFloat(), enabled = !submitting)
                        if (limits.maximumFadeUs != null) {
                            OutlinedTextField(fadeInText, onValueChange = { value -> fadeInText = value; updateAdjustments(inValue = value) },
                                label = { Text("Fade in") }, suffix = { Text("s") }, singleLine = true, enabled = !submitting,
                                isError = fadeIn == null, keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Decimal), modifier = Modifier.fillMaxWidth())
                            OutlinedTextField(fadeOutText, onValueChange = { value -> fadeOutText = value; updateAdjustments(outValue = value) },
                                label = { Text("Fade out") }, suffix = { Text("s") }, singleLine = true, enabled = !submitting,
                                isError = fadeOut == null, keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Decimal), modifier = Modifier.fillMaxWidth())
                        }
                        Text("Positive gain includes peak protection. Linear fades preserve duration.", style = MaterialTheme.typography.bodySmall)
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
                if(video && config.videoCodec == VideoCodec.COPY && (config.videoSize != VideoSize.ORIGINAL || config.frameRate != FrameRate.ORIGINAL)) {
                    Text("Copying requires original video geometry and timing.", color = MaterialTheme.colorScheme.error)
                    TextButton(enabled = !submitting, onClick = { onChange(config.copy(videoSize = VideoSize.ORIGINAL, frameRate = FrameRate.ORIGINAL, quality = 23u)) }) { Text("Use original video settings") }
                }
                OutlinedTextField(config.fileName, onValueChange = { onChange(config.copy(fileName = it)) }, label = { Text("Output name") }, suffix = { Text(".${format?.extension ?: ""}") },
                    singleLine = true, enabled = !submitting, isError = config.fileName.isBlank(), modifier = Modifier.fillMaxWidth().testTag("output-name"))
                Text("The source is checked again before acceptance. Incompatible tracks are reported instead of being discarded.", style = MaterialTheme.typography.bodySmall)
                Spacer(Modifier.height(12.dp))
            }
            Surface(shadowElevation = 3.dp) {
                Button(onClick = onConvert, enabled = !submitting && config.fileName.isNotBlank() && compressionEdit.error == null && customSizeValid && customRateValid && adjustmentsValid, modifier = Modifier.fillMaxWidth().padding(horizontal = 24.dp, vertical = 12.dp).heightIn(min = 48.dp)) {
                    if(submitting) { CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp); Spacer(Modifier.width(12.dp)) }
                    Text(if(submitting) "Validating request…" else "Convert")
                }
            }
        }
    }
    sheet?.takeUnless { submitting }?.let { title ->
        val values: List<Any> = when(title) {
            "Format" -> formats
            "Codec" -> format?.videoCodecs ?: emptyList<VideoCodec>()
            "Audio codec" -> format?.audioCodecs ?: emptyList<AudioCodec>()
            "Resolution" -> choices.videoSizes
            "Frame rate" -> choices.frameRates
            "Sample rate" -> choices.sampleRates
            "Channel layout" -> choices.channelLayouts
            "Sample precision" -> choices.audioPrecisions
            else -> emptyList<Any>()
        }
        val labels = values.map { value -> when(value) {
            is FormatOption -> value.label
            is VideoCodec -> videoCodec(value)
            is AudioCodec -> audioCodec(value)
            is VideoSize -> videoSize(value)
            is FrameRate -> frameRate(value)
            is AudioLayout -> audioLayout(value)
            is AudioPrecision -> audioPrecision(value)
            is UInt -> when(title) { "Sample rate" -> if(value == 0u) "Original" else "$value Hz"; else -> if(value == 0u) "Automatic" else "${value / 1000u} kbps" }
            else -> value.toString()
        } }
        val selected = values.indexOfFirst { value -> when(title) {
            "Format" -> (value as FormatOption).format == config.format
            "Codec" -> value == config.videoCodec
            "Audio codec" -> value == config.audioCodec
            "Resolution" -> value == config.videoSize
            "Frame rate" -> value == config.frameRate
            "Sample rate" -> value == (config.sampleRate ?: 0u)
            "Channel layout" -> value == selectedLayout
            "Sample precision" -> value == selectedPrecision
            else -> false
        } }
        OptionSheet(title, labels, selected, onSelect = { index ->
            if (submitting) return@OptionSheet
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
                "Channel layout" -> config.copy(channelLayout = (value as AudioLayout).takeUnless { it == AudioLayout.ORIGINAL }, channels = null)
                "Sample precision" -> config.copy(audioPrecision = (value as AudioPrecision).takeUnless { it == AudioPrecision.ORIGINAL })
                else -> config
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
private fun videoCodec(value: VideoCodec) = when(value) {
    VideoCodec.AUTOMATIC -> "Automatic"
    VideoCodec.H264 -> "H.264"
    VideoCodec.HEVC -> "HEVC / H.265"
    VideoCodec.AV1 -> "AV1"
    VideoCodec.VP8 -> "VP8"
    VideoCodec.VP9 -> "VP9"
    VideoCodec.MPEG4 -> "MPEG-4 Part 2"
    VideoCodec.FFV1 -> "FFV1"
    VideoCodec.MJPEG -> "MJPEG"
    VideoCodec.COPY -> "Copy compatible streams"
}
private fun audioCodec(value: AudioCodec) = when(value) {
    AudioCodec.AUTOMATIC -> "Automatic"
    AudioCodec.AAC -> "AAC-LC"
    AudioCodec.MP3 -> "MP3"
    AudioCodec.PCM_S16LE -> "PCM 16-bit"
    AudioCodec.PCM -> "PCM"
    AudioCodec.FLAC -> "FLAC"
    AudioCodec.ALAC -> "ALAC"
    AudioCodec.OPUS -> "Opus"
    AudioCodec.VORBIS -> "Vorbis"
    AudioCodec.AC3 -> "AC-3"
    AudioCodec.EAC3 -> "E-AC-3"
    AudioCodec.WAVPACK -> "WavPack"
    AudioCodec.WMA -> "WMA"
    AudioCodec.HE_AAC -> "HE-AAC"
    AudioCodec.AMR_NB -> "AMR-NB"
    AudioCodec.AMR_WB -> "AMR-WB"
    AudioCodec.COPY -> "Copy compatible streams"
}
private fun audioLayout(value: AudioLayout) = when(value) {
    AudioLayout.ORIGINAL -> "Original"
    AudioLayout.MONO -> "Mono"
    AudioLayout.STEREO -> "Stereo"
    AudioLayout.SURROUND51 -> "5.1 · back surrounds"
    AudioLayout.SURROUND51_SIDE -> "5.1 · side surrounds"
    AudioLayout.SURROUND71 -> "7.1 · side and back surrounds"
}
private fun audioPrecision(value: AudioPrecision) = when(value) {
    AudioPrecision.ORIGINAL -> "Original"
    AudioPrecision.PCM8 -> "8-bit integer PCM"
    AudioPrecision.PCM16 -> "16-bit integer PCM"
    AudioPrecision.PCM24 -> "24-bit integer PCM"
    AudioPrecision.PCM32 -> "32-bit integer PCM"
    AudioPrecision.FLOAT32 -> "32-bit floating point"
    AudioPrecision.FLOAT64 -> "64-bit floating point"
}
private fun duration(value: Long): String { val seconds = value.coerceAtLeast(0) / 1_000_000; return "${seconds / 60}:${(seconds % 60).toString().padStart(2, '0')}" }
private fun bytes(value: Long): String = formatByteCount(value.toULong())

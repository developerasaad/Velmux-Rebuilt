package com.velmux.ui.screens

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import com.velmux.ui.app.MediaSession
import com.velmux.ui.app.userMessage
import com.velmux.ui.components.OptionSheet
import java.math.BigDecimal
import uniffi.velmux_core.*

internal fun defaultCompression() = CompressionSettings(
    reduceFileSize = false, mode = CompressionMode.HIGH_QUALITY,
    optimizeResolution = false, optimizeFrameRate = false,
    target = null, customVideo = null, customAudio = null,
)

/** Retains only editing drafts; all ranges and typed controls come from Rust. */
internal class CompressionEditorState(initial: CompressionSettings?) {
    var targetText by mutableStateOf(initial?.target?.let {
        BigDecimal(it.amountMilli.toString()).movePointLeft(3).stripTrailingZeros().toPlainString()
    }.orEmpty())
    var targetUnit by mutableStateOf(initial?.target?.unit ?: TargetSizeUnit.MEGABYTES)
    var videoKind by mutableStateOf(initial?.customVideo?.presentation()?.first)
    var audioKind by mutableStateOf(initial?.customAudio?.presentation()?.first)
    val values = mutableStateMapOf<CompressionControlKind, String>().apply {
        initial?.customVideo?.presentation()?.let { (kind, value) -> put(kind, value.toString()) }
        initial?.customAudio?.presentation()?.let { (kind, value) -> put(kind, value.toString()) }
    }

    fun select(option: CompressionControlOption?, video: Boolean) {
        if (video) videoKind = option?.kind else audioKind = option?.kind
        if (option != null && option.kind !in values) values[option.kind] = option.defaultValue?.toString().orEmpty()
    }

    fun evaluate(media: MediaSession, settings: CompressionSettings, choices: CompressionChoices): CompressionEdit {
        // Disabled settings preserve the user's inactive drafts without applying
        // them. Active modes contain only their own target or rate controls.
        if (!settings.reduceFileSize) return CompressionEdit(settings, null)
        val clean = settings.copy(target = null, customVideo = null, customAudio = null)
        return try {
            when (settings.mode) {
                CompressionMode.TARGET_SIZE -> CompressionEdit(
                    clean.copy(target = media.parseTargetSize(targetText, targetUnit)), null,
                )
                CompressionMode.CUSTOM -> {
                    val video = videoKind?.let { kind ->
                        media.videoCompressionControl(kind, value(kind, choices.video))
                    }
                    val audio = audioKind?.let { kind ->
                        media.audioCompressionControl(kind, value(kind, choices.audio))
                    }
                    if (video == null && audio == null) CompressionEdit(clean, "Choose an applicable Custom control.")
                    else CompressionEdit(clean.copy(customVideo = video, customAudio = audio), null)
                }
                else -> CompressionEdit(clean, null)
            }
        } catch (failure: Exception) {
            CompressionEdit(clean, failure.userMessage("Enter an applicable compression value."))
        }
    }

    private fun value(kind: CompressionControlKind, choices: List<CompressionControlOption>): ULong {
        val option = choices.firstOrNull { it.kind == kind }
            ?: throw IllegalArgumentException("The retained Custom control is unavailable. Choose an applicable control or change the codec.")
        val value = values[kind]?.toULongOrNull()
            ?: throw IllegalArgumentException("Enter a whole-number value for ${option.label}.")
        require(value in option.minimum..option.maximum && (option.values.isEmpty() || value in option.values)) {
            "Choose an applicable value for ${option.label}."
        }
        return value
    }
}

internal data class CompressionEdit(val settings: CompressionSettings, val error: String?)

@Composable
internal fun CompressionControls(
    media: MediaSession, editor: CompressionEditorState, settings: CompressionSettings,
    choices: CompressionChoices, video: Boolean, enabled: Boolean,
    evaluation: CompressionEdit, onChange: (CompressionSettings) -> Unit,
) {
    var sheet by remember { mutableStateOf<String?>(null) }
    fun commit(base: CompressionSettings = settings) {
        onChange(editor.evaluate(media, base, choices).settings)
    }
    Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
        CompressionSwitch("Reduce File Size", settings.reduceFileSize, enabled, "reduce-file-size") {
            commit(settings.copy(reduceFileSize = it))
        }
        if (settings.reduceFileSize) {
            CompressionChoice("Compression", modeLabel(settings.mode), enabled) { sheet = "mode" }
            when (settings.mode) {
                CompressionMode.TARGET_SIZE -> {
                    OutlinedTextField(editor.targetText, onValueChange = { editor.targetText = it; commit() },
                        label = { Text("Target file size") }, singleLine = true, enabled = enabled,
                        isError = evaluation.error != null,
                        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Decimal),
                        modifier = Modifier.fillMaxWidth().testTag("target-size")
                            .semantics { contentDescription = "Target file size" })
                    CompressionChoice("Unit", unitLabel(editor.targetUnit), enabled) { sheet = "unit" }
                    Text("KB = 1,000 bytes · MB = 1,000,000 bytes · GB = 1,000,000,000 bytes. Actual final size is checked; a target is not a guarantee.", style = MaterialTheme.typography.bodySmall)
                }
                CompressionMode.CUSTOM -> {
                    if (video) ControlEditor("Video", editor.videoKind, editor, choices.video, enabled, { sheet = "video" }, { commit() })
                    ControlEditor("Audio", editor.audioKind, editor, choices.audio, enabled, { sheet = "audio" }, { commit() })
                    if (choices.video.isEmpty() && choices.audio.isEmpty()) {
                        Text("No Custom control applies to these output settings. Choose a preset or change the codec.", style = MaterialTheme.typography.bodySmall)
                    }
                }
                else -> Unit
            }
            if (video) {
                CompressionSwitch("Optimize resolution", settings.optimizeResolution, enabled, "optimize-resolution") {
                    commit(settings.copy(optimizeResolution = it))
                }
                CompressionSwitch("Optimize frame rate", settings.optimizeFrameRate, enabled, "optimize-frame-rate") {
                    commit(settings.copy(optimizeFrameRate = it))
                }
                Text("Optimization requires Original resolution or frame rate. Explicit selections remain unchanged.", style = MaterialTheme.typography.bodySmall)
            }
            evaluation.error?.let { Text(it, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall) }
        } else {
            Text("Preserve compatible streams. Necessary encoding uses a high-fidelity policy.", style = MaterialTheme.typography.bodySmall)
        }
    }
    sheet?.takeIf { enabled }?.let { selected ->
        when (selected) {
            "mode" -> {
                val modes = listOf(CompressionMode.HIGH_QUALITY, CompressionMode.BALANCED, CompressionMode.SMALLER_FILE, CompressionMode.TARGET_SIZE, CompressionMode.CUSTOM)
                OptionSheet("Compression", modes.map(::modeLabel), modes.indexOf(settings.mode), {
                    commit(settings.copy(mode = modes[it])); sheet = null
                }, { sheet = null })
            }
            "unit" -> {
                val units = listOf(TargetSizeUnit.KILOBYTES, TargetSizeUnit.MEGABYTES, TargetSizeUnit.GIGABYTES)
                OptionSheet("Target size unit", units.map(::unitLabel), units.indexOf(editor.targetUnit), {
                    editor.targetUnit = units[it]; commit(); sheet = null
                }, { sheet = null })
            }
            else -> {
                val isVideo = selected == "video"
                val options = if (isVideo) choices.video else choices.audio
                val kind = if (isVideo) editor.videoKind else editor.audioKind
                OptionSheet(if (isVideo) "Video rate control" else "Audio rate control",
                    listOf("Encoder default") + options.map { it.label },
                    if (kind == null) 0 else options.indexOfFirst { it.kind == kind }.takeIf { it >= 0 }?.plus(1) ?: -1,
                    { index -> editor.select(options.getOrNull(index - 1), isVideo); commit(); sheet = null },
                    { sheet = null })
            }
        }
    }
}

@Composable
private fun ControlEditor(
    title: String, kind: CompressionControlKind?, editor: CompressionEditorState,
    choices: List<CompressionControlOption>, enabled: Boolean, onSelect: () -> Unit, onEdit: () -> Unit,
) {
    val option = choices.firstOrNull { it.kind == kind }
    if (choices.isNotEmpty() || kind != null) {
        CompressionChoice("$title rate control", option?.label ?: if (kind == null) "Encoder default" else "Retained unavailable control", enabled, onSelect)
    }
    if (kind != null && option != null) {
        val text = editor.values[kind].orEmpty()
        val value = text.toULongOrNull()
        val valid = value != null && value in option.minimum..option.maximum && (option.values.isEmpty() || value in option.values)
        OutlinedTextField(text, onValueChange = { editor.values[kind] = it; onEdit() },
            label = { Text(option.label) }, suffix = { Text(option.unit) },
            supportingText = { Text(if (option.values.isEmpty()) "${option.minimum}–${option.maximum}" else option.values.joinToString(", ")) },
            singleLine = true, enabled = enabled, isError = !valid,
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
            modifier = Modifier.fillMaxWidth().testTag(if (title == "Video") "custom-video-value" else "custom-audio-value"))
    }
}

@Composable
private fun CompressionSwitch(title: String, checked: Boolean, enabled: Boolean, tag: String, onChange: (Boolean) -> Unit) {
    Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
        Text(title, Modifier.weight(1f))
        Switch(checked = checked, onCheckedChange = onChange, enabled = enabled,
            modifier = Modifier.testTag(tag).semantics { contentDescription = title })
    }
}

@Composable
private fun CompressionChoice(title: String, value: String, enabled: Boolean, onClick: () -> Unit) {
    OutlinedButton(onClick, enabled = enabled,
        modifier = Modifier.fillMaxWidth().heightIn(min = 48.dp).semantics { contentDescription = title }) {
        Text(title, Modifier.weight(1f)); Text(value, Modifier.padding(start = 12.dp))
    }
}

private fun modeLabel(mode: CompressionMode) = when (mode) {
    CompressionMode.HIGH_QUALITY -> "High Quality"
    CompressionMode.BALANCED -> "Balanced Quality"
    CompressionMode.SMALLER_FILE -> "Smaller File"
    CompressionMode.TARGET_SIZE -> "Target File Size"
    CompressionMode.CUSTOM -> "Custom"
}

private fun unitLabel(unit: TargetSizeUnit) = when (unit) {
    TargetSizeUnit.KILOBYTES -> "KB"
    TargetSizeUnit.MEGABYTES -> "MB"
    TargetSizeUnit.GIGABYTES -> "GB"
}

// This is presentation of stored typed intent, not codec selection or policy.
private fun VideoRateControl.presentation(): Pair<CompressionControlKind, ULong> = when (this) {
    is VideoRateControl.AverageBitrate -> CompressionControlKind.VIDEO_AVERAGE_BITRATE to bitsPerSecond
    is VideoRateControl.H264Crf -> CompressionControlKind.H264_CRF to value.toULong()
    is VideoRateControl.HevcCrf -> CompressionControlKind.HEVC_CRF to value.toULong()
    is VideoRateControl.Av1Crf -> CompressionControlKind.AV1_CRF to value.toULong()
    is VideoRateControl.Vp8Cq -> CompressionControlKind.VP8_CQ to value.toULong()
    is VideoRateControl.Vp9Crf -> CompressionControlKind.VP9_CRF to value.toULong()
    is VideoRateControl.Mpeg4Qscale -> CompressionControlKind.MPEG4_QSCALE to value.toULong()
    is VideoRateControl.MjpegQscale -> CompressionControlKind.MJPEG_QSCALE to value.toULong()
    is VideoRateControl.Ffv1Context -> CompressionControlKind.FFV1_CONTEXT to if (large) 1uL else 0uL
}

private fun AudioRateControl.presentation(): Pair<CompressionControlKind, ULong> = when (this) {
    is AudioRateControl.AverageBitrate -> CompressionControlKind.AUDIO_AVERAGE_BITRATE to bitsPerSecond.toULong()
    is AudioRateControl.Mp3VbrQuality -> CompressionControlKind.MP3_VBR_QUALITY to value.toULong()
    is AudioRateControl.VorbisQuality -> CompressionControlKind.VORBIS_QUALITY to value.toULong()
    is AudioRateControl.FlacLevel -> CompressionControlKind.FLAC_LEVEL to value.toULong()
    is AudioRateControl.AlacLevel -> CompressionControlKind.ALAC_LEVEL to value.toULong()
    is AudioRateControl.WavpackLevel -> CompressionControlKind.WAVPACK_LEVEL to value.toULong()
}

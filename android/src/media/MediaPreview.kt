package com.velmux.ui.app

import androidx.compose.foundation.layout.Column
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.viewinterop.AndroidView
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.media3.common.MediaItem
import androidx.media3.common.PlaybackException
import androidx.media3.common.Player
import androidx.media3.common.util.UnstableApi
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.ui.PlayerView

@androidx.annotation.OptIn(UnstableApi::class)
@Composable
fun MediaPreview(locator: String, modifier: Modifier, onUnsupported: () -> Unit) {
    val context = LocalContext.current
    val owner = LocalLifecycleOwner.current
    val latestUnsupported by rememberUpdatedState(onUnsupported)
    var errorMessage by remember(locator) { mutableStateOf<String?>(null) }
    val player = remember(locator, context, owner) { ExoPlayer.Builder(context).build() }
    DisposableEffect(player, owner) {
        var reportedUnsupported = false
        val listener = object : Player.Listener {
            override fun onPlayerError(error: PlaybackException) {
                val unsupported = error.errorCode in setOf(
                    PlaybackException.ERROR_CODE_DECODER_INIT_FAILED,
                    PlaybackException.ERROR_CODE_DECODER_QUERY_FAILED,
                    PlaybackException.ERROR_CODE_DECODING_FAILED,
                    PlaybackException.ERROR_CODE_DECODING_FORMAT_EXCEEDS_CAPABILITIES,
                    PlaybackException.ERROR_CODE_DECODING_FORMAT_UNSUPPORTED,
                    PlaybackException.ERROR_CODE_PARSING_CONTAINER_UNSUPPORTED,
                )
                errorMessage = "Preview is unavailable for this source."
                if (unsupported && !reportedUnsupported) {
                    reportedUnsupported = true
                    latestUnsupported()
                }
            }
        }
        val lifecycle = LifecycleEventObserver { _, event ->
            if (event == Lifecycle.Event.ON_STOP) player.pause()
        }
        player.addListener(listener)
        owner.lifecycle.addObserver(lifecycle)
        player.setMediaItem(MediaItem.fromUri(locator))
        player.prepare()
        onDispose {
            owner.lifecycle.removeObserver(lifecycle)
            player.removeListener(listener)
            player.release()
        }
    }
    Column {
        AndroidView(
            modifier = modifier,
            factory = { viewContext ->
                PlayerView(viewContext).apply {
                    useController = true
                    controllerAutoShow = true
                    controllerShowTimeoutMs = 0
                    setShowNextButton(false)
                    setShowPreviousButton(false)
                    setShowBuffering(PlayerView.SHOW_BUFFERING_WHEN_PLAYING)
                    this.player = player
                }
            },
            update = { it.player = player },
            onRelease = { it.player = null },
        )
        errorMessage?.let { Text(it, style = MaterialTheme.typography.bodySmall) }
    }
}

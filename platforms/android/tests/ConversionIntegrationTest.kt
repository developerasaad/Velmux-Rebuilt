package com.velmux.mobile

import android.media.AudioFormat
import android.media.MediaCodec
import android.media.MediaExtractor
import android.media.MediaFormat
import android.media.MediaMetadataRetriever
import android.net.Uri
import android.graphics.Rect
import android.graphics.Color
import android.view.View
import android.view.ViewGroup
import android.os.SystemClock
import android.os.Build
import android.os.Bundle
import android.util.Log
import android.provider.DocumentsContract
import androidx.media3.common.MediaItem
import androidx.media3.common.PlaybackException
import androidx.media3.common.Player
import androidx.media3.common.util.UnstableApi
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.exoplayer.video.VideoFrameMetadataListener
import androidx.media3.ui.PlayerView
import androidx.test.core.app.ActivityScenario
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import androidx.test.uiautomator.By
import androidx.test.uiautomator.UiDevice
import androidx.test.uiautomator.UiScrollable
import androidx.test.uiautomator.UiSelector
import androidx.test.uiautomator.UiObject2
import androidx.test.uiautomator.Until
import com.velmux.ui.app.JobSnapshot
import org.junit.Assert.*
import org.junit.Test
import org.junit.Rule
import org.junit.rules.TestWatcher
import org.junit.runner.Description
import org.junit.runner.RunWith
import uniffi.velmux_core.AudioCodec
import uniffi.velmux_core.EngineException
import uniffi.velmux_core.OutputFormat
import java.io.ByteArrayOutputStream
import java.io.File
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.security.MessageDigest
import java.util.UUID
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicReference
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong
import kotlin.math.abs
import kotlin.math.sqrt

@RunWith(AndroidJUnit4::class)
class ConversionIntegrationTest {
    private val instrumentation get() = InstrumentationRegistry.getInstrumentation()
    private val context get() = instrumentation.targetContext
    private val media get() = (context.applicationContext as VelmuxApplication).session.media
    private val resolver get() = context.contentResolver
    private val provider = Uri.parse("content://com.velmux.mobile.test.control")
    private val device get() = UiDevice.getInstance(instrumentation)

    @get:Rule
    val retainFailureEvidence = object : TestWatcher() {
        override fun failed(error: Throwable, description: Description) {
            runCatching { capture("failure-${description.methodName.take(60)}.png") }
            runCatching { retainText("failure-${description.methodName.take(60)}.txt", error.stackTraceToString()) }
        }
    }

    @Test
    fun systemPickerCancelThenVideoConversionPublishesDecodedOutput() {
        media.beginNative()
        val originalAppearance = media.appearanceNative()
        val before = media.jobsNative().map { it.id }.toSet()
        val uri = grant(PICKER_VIDEO)
        assertEquals("The generic-data fixture must not advertise a video MIME type", "application/octet-stream", resolver.getType(uri))
        val sourceHash = hash(uri)
        revoke(PICKER_VIDEO)
        assertFalse(resolver.persistedUriPermissions.any { it.uri == uri })
        try {
            ActivityScenario.launch(MainActivity::class.java).use { scenario ->
                awaitText("Video Converter")
                capture("ui-home.png")
                clickText("Video Converter")
                awaitPicker()
                device.pressBack()
                awaitText("Video Converter")
                assertFalse("Picker cancellation must not become a source error", device.hasObject(By.text("Source unavailable")))
                assertEquals(before, media.jobsNative().map { it.id }.toSet())
                clickText("Video Converter")
                chooseDocument(PICKER_VIDEO)
                awaitText(PICKER_VIDEO)
                awaitText("Convert", exact = true)
                assertTrue("The picker result must have durable read permission",
                    resolver.persistedUriPermissions.any { it.uri == uri && it.isReadPermission })
                clickText("Codec")
                clickText("H.264")
                capture("ui-video-workspace.png")
                exerciseEmbeddedPlayerControls(scenario, PICKER_VIDEO)
                exerciseResponsiveWorkspace(PICKER_VIDEO)
                exerciseInvalidOutputName(before)
                clickText("Convert", exact = true)
                val submitted = awaitNewJob(before)
                val completed = awaitJob(submitted.id)
                assertEquals("completed", completed.state)
                val output = Uri.parse(requireNotNull(completed.outputLocator))
                verifyVideo(output)
                retainOutput("android-video-converted.mp4", output)
                assertEquals(sourceHash, hash(uri))
                awaitText("Completed", exact = true)
                capture("ui-jobs.png")
                clickText("Back", exact = true)
                clickText("Settings", exact = true)
                clickText("Dark", exact = true)
                waitUntil("Appearance was not committed") { media.appearanceNative() == "dark" }
                awaitRenderedDarkAppearance()
                capture("ui-settings-dark.png")
                scenario.recreate()
                awaitText("Video Converter", exact = true)
                clickText("Settings", exact = true)
                awaitRenderedDarkAppearance()
                capture("ui-settings-dark-restored.png")
            }
            val accepted = media.jobsNative().filter { it.id !in before }
            assertEquals("One Convert action must produce one accepted job", 1, accepted.size)
        } finally {
            runCatching { media.setAppearanceNative(originalAppearance) }
            cleanupNewJobs(before)
            revoke(PICKER_VIDEO)
        }
    }

    @Test
    fun publicFacadeAudioEncodingIsImmutableIdempotentAndIndependentlyDecoded() {
        media.beginNative()
        val uri = grant("stereo.wav")
        val beforeHash = hash(uri)
        val inspected = media.inspectNative(uri.toString(), "stereo.wav", false)
        val acceptedIds = mutableListOf<String>()
        try {
            val defaults = media.defaultsNative(inspected.id, false)
            val beforeInvalid = media.jobsNative().size
            try {
                media.submitNative(inspected.id, UUID.randomUUID().toString(), defaults.copy(sampleRate = 12_345u))
                fail("The core accepted an unsupported rate submitted without UI constraints")
            } catch (_: EngineException.Configuration) { }
            assertEquals(beforeInvalid, media.jobsNative().size)

            for ((format, codec) in listOf(OutputFormat.M4A to AudioCodec.AAC, OutputFormat.FLAC to AudioCodec.FLAC)) {
                val stem = "native-audio-${UUID.randomUUID()}"
                val configuration = defaults.copy(format = format, audioCodec = codec, fileName = stem)
                val facts = "Source: ${inspected.info}\nAccepted intent: $configuration\n"
                Log.i("VelmuxMediaVerification", facts.take(3000))
                retainText("audio-${format.name.lowercase()}-facts.txt", facts)
                val key = UUID.randomUUID().toString()
                val accepted = media.submitNative(inspected.id, key, configuration)
                acceptedIds.add(accepted.id)
                assertEquals(accepted.id, media.submitNative(inspected.id, key, configuration).id)
                try {
                    media.submitNative(inspected.id, key, configuration.copy(fileName = "changed-after-acceptance"))
                    fail("An accepted idempotency key was reused for a changed request")
                } catch (_: EngineException.Configuration) { }
                val completed = awaitJob(accepted.id)
                assertTrue(completed.fileName.startsWith(stem))
                val output = Uri.parse(requireNotNull(completed.outputLocator))
                val expectedMime = if (format == OutputFormat.M4A) "audio/mp4a-latm" else "audio/flac"
                val formats = tracks(output)
                assertEquals(1, formats.size)
                val extractedMime = formats.single().getString(MediaFormat.KEY_MIME)
                if (format == OutputFormat.FLAC) {
                    verifyFlacRepresentation(output)
                    // Android's FLAC extractor may decode during extraction and expose raw PCM.
                    assertTrue("Unexpected native FLAC track representation: $extractedMime",
                        extractedMime == "audio/flac" || extractedMime == "audio/raw")
                } else assertEquals(expectedMime, extractedMime)
                assertEquals(48_000, formats.single().getInteger(MediaFormat.KEY_SAMPLE_RATE))
                assertEquals(2, formats.single().getInteger(MediaFormat.KEY_CHANNEL_COUNT))
                assertDuration(formats.single())
                val decoded = decodeAudio(output)
                verifyTones(decoded)
                if (format == OutputFormat.FLAC) {
                    val originalPcm = instrumentation.context.assets.open("stereo.wav").use { wavPcm(it.readBytes()) }
                    assertArrayEquals("Lossless conversion changed the known source PCM samples", originalPcm, decoded)
                }
                retainOutput(if (format == OutputFormat.FLAC) "android-audio.flac" else "android-audio.m4a", output)
                assertEquals(1, media.jobsNative().count { it.id == accepted.id })
            }
            assertEquals(beforeHash, hash(uri))
        } finally {
            media.releaseNative(inspected.id)
            acceptedIds.forEach { cleanupJob(it) }
            revoke("stereo.wav")
        }
    }

    @Test
    fun coreInspectionReferencesOwnDurableAccessUntilLastRelease() {
        media.beginNative()
        val uri = grantTransient("video-large.mp4")
        val original = hash(uri)
        val first = media.inspectNative(uri.toString(), "video-large.mp4", true)
        var secondId: String? = null
        fun hasRead() = resolver.persistedUriPermissions.any { it.uri == uri && it.isReadPermission }
        try {
            val second = media.inspectNative(uri.toString(), "video-large.mp4", true)
            secondId = second.id
            assertTrue("The first core reference did not acquire durable access", hasRead())
            media.releaseNative(first.id)
            assertTrue("A remaining live inspection lost source access", hasRead())
            assertEquals(original, hash(uri))
            media.releaseNative(second.id)
            waitUntil("The last released inspection retained its document permission", 5_000) { !hasRead() }
            // The original still exists and can be granted again; cleanup changed only permission.
            grantTransient("video-large.mp4")
            assertEquals(original, hash(uri))
        } finally {
            runCatching { media.releaseNative(first.id) }
            secondId?.let { id -> runCatching { media.releaseNative(id) } }
            revoke("video-large.mp4")
        }
    }

    @androidx.annotation.OptIn(UnstableApi::class)
    @Test
    fun media3PreparesRendersAndSeeksTheRealProviderSource() {
        val uri = grant("video.mp4")
        val ready = CountDownLatch(1)
        val rendered = CountDownLatch(1)
        val sought = CountDownLatch(1)
        val failure = AtomicReference<PlaybackException?>(null)
        val seeking = AtomicBoolean(false)
        var player: ExoPlayer? = null
        try {
            ActivityScenario.launch(MainActivity::class.java).use { scenario ->
                scenario.onActivity { activity ->
                    player = ExoPlayer.Builder(activity).build().also { native ->
                        native.addListener(object : Player.Listener {
                            override fun onPlaybackStateChanged(playbackState: Int) {
                                if (playbackState == Player.STATE_READY) ready.countDown()
                            }
                            override fun onRenderedFirstFrame() { rendered.countDown() }
                            override fun onPlayerError(error: PlaybackException) {
                                failure.set(error); ready.countDown(); rendered.countDown(); sought.countDown()
                            }
                        })
                        native.setVideoFrameMetadataListener { presentationTimeUs, _, _, _ ->
                            if (seeking.get() && presentationTimeUs in 900_000..1_500_000) sought.countDown()
                        }
                        activity.setContentView(PlayerView(activity).apply {
                            useController = true
                            controllerShowTimeoutMs = 0
                            this.player = native
                        })
                        native.setMediaItem(MediaItem.fromUri(uri))
                        native.prepare()
                    }
                }
                assertTrue("Media3 did not become ready", ready.await(30, TimeUnit.SECONDS))
                assertNull("Media3 failed to prepare the provider source", failure.get())
                assertTrue("No video frame was rendered", rendered.await(30, TimeUnit.SECONDS))
                assertNull("Media3 failed to render the provider source", failure.get())
                instrumentation.runOnMainSync {
                    val native = requireNotNull(player)
                    assertTrue(native.duration in 1_900..2_200)
                    seeking.set(true)
                    native.seekTo(1_000)
                }
                assertTrue("The player did not acknowledge an actual seek", sought.await(15, TimeUnit.SECONDS))
                assertNull(failure.get())
                instrumentation.runOnMainSync {
                    assertTrue(requireNotNull(player).currentPosition >= 900)
                    requireNotNull(player).play()
                }
                waitUntil("Playback did not advance after seeking", 15_000) {
                    var advanced = false
                    instrumentation.runOnMainSync { advanced = requireNotNull(player).currentPosition >= 1_200 }
                    advanced
                }
                instrumentation.runOnMainSync {
                    requireNotNull(player).pause()
                    assertFalse(requireNotNull(player).playWhenReady)
                }
                capture("ui-native-player-seek.png")
                instrumentation.runOnMainSync { player?.release(); player = null }
            }
        } finally {
            instrumentation.runOnMainSync { player?.release() }
            revoke("video.mp4")
        }
    }

    @androidx.annotation.OptIn(UnstableApi::class)
    private fun exerciseEmbeddedPlayerControls(scenario: ActivityScenario<MainActivity>, sourceName: String) {
        reachableText(sourceName, exact = true)
        var embedded: PlayerView? = null
        waitUntil("The converter's embedded player did not prepare", 30_000) {
            var prepared = false
            scenario.onActivity { activity ->
                embedded = findPlayerView(activity.window.decorView)
                prepared = embedded?.player?.playbackState == Player.STATE_READY
            }
            prepared
        }
        fun clickControl(description: String) {
            val controlBounds = Rect()
            var visible = false
            scenario.onActivity {
                val control = requireNotNull(embedded?.findViewById<View>(androidx.media3.ui.R.id.exo_play_pause))
                visible = control.getGlobalVisibleRect(controlBounds)
            }
            if (!visible) {
                val bounds = Rect()
                scenario.onActivity { assertTrue(requireNotNull(embedded).getGlobalVisibleRect(bounds)) }
                device.click(bounds.centerX(), bounds.centerY())
            }
            waitUntil("The embedded player did not expose its $description control", 3_000) {
                var matches = false
                scenario.onActivity {
                    val control = requireNotNull(embedded?.findViewById<View>(androidx.media3.ui.R.id.exo_play_pause))
                    matches = control.contentDescription?.toString() == description && control.getGlobalVisibleRect(controlBounds)
                }
                matches
            }
            // Read the actual native control's bounds without a slow complete
            // accessibility-tree traversal: this fixture is only two seconds.
            assertTrue(device.click(controlBounds.centerX(), controlBounds.centerY()))
        }
        try {
            clickControl("Play")
            waitUntil("Embedded playback did not advance after the real Play control", 10_000) {
                var advancing = false
                scenario.onActivity { advancing = requireNotNull(embedded?.player).currentPosition >= 150 }
                advancing
            }
            clickControl("Pause")
            waitUntil("The real Pause control did not pause playback", 3_000) {
                var paused = false
                scenario.onActivity { paused = !requireNotNull(embedded?.player).playWhenReady }
                paused
            }
            val frameTimestamp = AtomicLong(-1)
            val listener = VideoFrameMetadataListener { presentationTimeUs, _, _, _ -> frameTimestamp.set(presentationTimeUs) }
            var native: ExoPlayer? = null
            runCatching {
                UiScrollable(UiSelector().scrollable(true)).setMaxSearchSwipes(6)
                    .scrollIntoView(UiSelector().resourceIdMatches(".*:id/exo_progress"))
            }
            val timebar = Rect()
            scenario.onActivity {
                native = requireNotNull(embedded?.player) as ExoPlayer
                native?.setVideoFrameMetadataListener(listener)
                val progress = requireNotNull(embedded?.findViewById<View>(androidx.media3.ui.R.id.exo_progress))
                assertTrue("The native seek bar is not reachable in the workspace", progress.getGlobalVisibleRect(timebar))
                assertTrue(timebar.width() > 40 && timebar.height() > 0)
            }
            try {
                // Tap the actual native timebar; observing a decoded presentation
                // timestamp proves this is more than a displayed seek position.
                device.click(timebar.left + timebar.width() * 2 / 3, timebar.centerY())
                waitUntil("The embedded native seek control did not decode the requested interval", 10_000) {
                    frameTimestamp.get() in 900_000L..1_900_000L
                }
                scenario.onActivity { assertTrue(requireNotNull(embedded?.player).currentPosition in 900L..1_900L) }
                capture("ui-video-embedded-seek.png")
            } finally {
                scenario.onActivity { native?.clearVideoFrameMetadataListener(listener) }
            }
        } catch (error: Throwable) {
            runCatching { capture("failure-embedded-player.png") }
            throw error
        }
    }

    private fun exerciseInvalidOutputName(before: Set<String>) {
        reachableText("Output name", exact = true)
        val field = requireNotNull(device.findObjects(By.clazz("android.widget.EditText"))
            .firstOrNull { it.text.isNotBlank() && it.text.toIntOrNull() == null }) {
            "The output name field is not reachable"
        }
        val validName = field.text
        field.text = "../invalid-output"
        clickText("Convert", exact = true)
        awaitText("Conversion was not accepted", exact = true)
        awaitText("Use a filename")
        assertTrue("The shared alert exposed generated binding field labels",
            device.hasObject(By.text("Use a filename of 1–180 bytes without path separators or control characters.")))
        assertEquals("Rejected configuration created a job", before, media.jobsNative().map { it.id }.toSet())
        capture("ui-invalid-output-alert.png")
        clickText("OK", exact = true)
        assertFalse("The dismissed error remained visible", device.hasObject(By.text("Conversion was not accepted")))
        reachableText("Output name", exact = true)
        requireNotNull(device.findObject(By.clazz("android.widget.EditText").text("../invalid-output"))).text = validName
        awaitText("Convert", exact = true)
    }

    private fun findPlayerView(view: View): PlayerView? {
        if (view is PlayerView) return view
        if (view is ViewGroup) for (index in 0 until view.childCount) {
            findPlayerView(view.getChildAt(index))?.let { return it }
        }
        return null
    }

    private fun exerciseResponsiveWorkspace(sourceName: String) {
        val originalFontScale = device.executeShellCommand("settings get system font_scale").trim().toFloatOrNull()
            ?.takeIf { it.isFinite() && it > 0 }?.toString() ?: "1.0"
        val originalRotation = device.executeShellCommand("settings get system user_rotation").trim().toIntOrNull() ?: 0
        val originalAutoRotation = device.executeShellCommand("settings get system accelerometer_rotation").trim().toIntOrNull() ?: 0
        var phase = "landscape"
        try {
            device.setOrientationLeft()
            waitUntil("The emulator did not enter landscape", 10_000) { device.displayWidth > device.displayHeight }
            awaitText("Convert", exact = true)
            reachableText(sourceName, exact = true)
            capture("ui-video-landscape.png")
            clickText("Codec")
            awaitText("H.264")
            clickText("H.264")
            assertTrue("Convert became unreachable in landscape", device.hasObject(By.text("Convert")))
            device.setOrientationNatural()
            waitUntil("The emulator did not return to portrait", 10_000) { device.displayHeight > device.displayWidth }
            phase = "large-font"
            device.executeShellCommand("settings put system font_scale 1.3")
            waitUntil("The emulator did not apply 1.3 font scale", 10_000) { abs(context.resources.configuration.fontScale - 1.3f) < 0.01f }
            awaitText("Convert", exact = true)
            reachableText(sourceName, exact = true)
            capture("ui-video-large-font.png")
            clickText("Format")
            awaitText("MP4", exact = true)
            clickText("MP4", exact = true)
            assertTrue("Convert became unreachable at 1.3 font scale", device.hasObject(By.text("Convert")))
        } catch (error: Throwable) {
            runCatching { capture("failure-responsive-$phase.png") }
            throw error
        } finally {
            device.executeShellCommand("settings put system font_scale $originalFontScale")
            device.setOrientationNatural()
            device.unfreezeRotation()
            device.executeShellCommand("settings put system user_rotation $originalRotation")
            device.executeShellCommand("settings put system accelerometer_rotation $originalAutoRotation")
        }
        awaitText("Convert", exact = true)
    }

    private fun verifyVideo(uri: Uri) {
        val formats = tracks(uri)
        val video = formats.single { it.getString(MediaFormat.KEY_MIME)?.startsWith("video/") == true }
        val audio = formats.single { it.getString(MediaFormat.KEY_MIME)?.startsWith("audio/") == true }
        assertEquals(2, formats.size)
        assertEquals("video/avc", video.getString(MediaFormat.KEY_MIME))
        assertEquals(320, video.getInteger(MediaFormat.KEY_WIDTH))
        assertEquals(180, video.getInteger(MediaFormat.KEY_HEIGHT))
        assertDuration(video)
        assertEquals("audio/mp4a-latm", audio.getString(MediaFormat.KEY_MIME))
        assertEquals(48_000, audio.getInteger(MediaFormat.KEY_SAMPLE_RATE))
        assertEquals(2, audio.getInteger(MediaFormat.KEY_CHANNEL_COUNT))
        assertDuration(audio)
        val retriever = MediaMetadataRetriever()
        try {
            retriever.setDataSource(context, uri)
            val first = requireNotNull(retriever.getFrameAtTime(500_000, MediaMetadataRetriever.OPTION_CLOSEST))
            val second = requireNotNull(retriever.getFrameAtTime(1_500_000, MediaMetadataRetriever.OPTION_CLOSEST))
            try {
                assertEquals(320, first.width); assertEquals(180, first.height)
                val firstPixels = IntArray(first.width * first.height)
                val secondPixels = IntArray(second.width * second.height)
                first.getPixels(firstPixels, 0, first.width, 0, 0, first.width, first.height)
                second.getPixels(secondPixels, 0, second.width, 0, 0, second.width, second.height)
                assertTrue("Decoded output is a blank frame", firstPixels.toSet().size > 8)
                assertFalse("The moving test pattern became a frozen frame", firstPixels.contentEquals(secondPixels))
            } finally { first.recycle(); second.recycle() }
        } finally { retriever.release() }
        verifyTones(decodeAudio(uri))
    }

    private fun verifyFlacRepresentation(uri: Uri) {
        val header = ByteArray(42)
        resolver.openInputStream(uri)!!.use { input ->
            var offset = 0
            while (offset < header.size) {
                val count = input.read(header, offset, header.size - offset)
                assertTrue("The FLAC STREAMINFO header is truncated", count > 0)
                offset += count
            }
        }
        assertEquals("fLaC", String(header, 0, 4, Charsets.US_ASCII))
        assertEquals("STREAMINFO must be the first FLAC metadata block", 0, header[4].toInt() and 0x7f)
        val blockLength = ((header[5].toInt() and 0xff) shl 16) or
            ((header[6].toInt() and 0xff) shl 8) or (header[7].toInt() and 0xff)
        assertEquals(34, blockLength)
        val stream = ByteBuffer.wrap(header, 18, 8).order(ByteOrder.BIG_ENDIAN).long
        assertEquals(48_000L, (stream ushr 44) and 0xfffff)
        assertEquals(2L, ((stream ushr 41) and 7) + 1)
        assertEquals(16L, ((stream ushr 36) and 31) + 1)
        assertEquals("Lossless output changed the independently known sample count", 96_000L, stream and 0xfffffffffL)
    }

    private fun tracks(uri: Uri): List<MediaFormat> {
        val extractor = extractor(uri)
        try { return (0 until extractor.trackCount).map { extractor.getTrackFormat(it) } }
        finally { extractor.release() }
    }

    private fun extractor(uri: Uri): MediaExtractor = MediaExtractor().also { extractor ->
        try {
            resolver.openAssetFileDescriptor(uri, "r")!!.use { descriptor ->
                extractor.setDataSource(descriptor.fileDescriptor, descriptor.startOffset, descriptor.length)
            }
        } catch (error: Throwable) { extractor.release(); throw error }
    }

    private fun decodeAudio(uri: Uri): ByteArray {
        val extractor = extractor(uri)
        var decoder: MediaCodec? = null
        try {
            val index = (0 until extractor.trackCount).first { extractor.getTrackFormat(it).getString(MediaFormat.KEY_MIME)?.startsWith("audio/") == true }
            val format = extractor.getTrackFormat(index)
            extractor.selectTrack(index)
            if (format.getString(MediaFormat.KEY_MIME) == "audio/raw") {
                assertEquals(48_000, format.getInteger(MediaFormat.KEY_SAMPLE_RATE))
                assertEquals(2, format.getInteger(MediaFormat.KEY_CHANNEL_COUNT))
                if (format.containsKey(MediaFormat.KEY_PCM_ENCODING)) {
                    assertEquals(AudioFormat.ENCODING_PCM_16BIT, format.getInteger(MediaFormat.KEY_PCM_ENCODING))
                }
                val pcm = ByteArrayOutputStream()
                val buffer = ByteBuffer.allocate(256 * 1024)
                val deadline = SystemClock.elapsedRealtime() + 30_000
                while (true) {
                    assertTrue("Native FLAC extraction exceeded its deadline", SystemClock.elapsedRealtime() < deadline)
                    buffer.clear()
                    val count = extractor.readSampleData(buffer, 0)
                    if (count < 0) break
                    assertTrue("Native FLAC extraction exceeded its PCM budget", pcm.size() + count <= 1_000_000)
                    val chunk = ByteArray(count)
                    buffer.position(0); buffer.get(chunk)
                    pcm.write(chunk)
                    if (!extractor.advance()) break
                }
                return pcm.toByteArray()
            }
            format.setInteger(MediaFormat.KEY_PCM_ENCODING, AudioFormat.ENCODING_PCM_16BIT)
            val codec = MediaCodec.createDecoderByType(requireNotNull(format.getString(MediaFormat.KEY_MIME)))
            decoder = codec
            codec.configure(format, null, null, 0)
            codec.start()
            val output = ByteArrayOutputStream()
            val info = MediaCodec.BufferInfo()
            var inputDone = false
            var outputDone = false
            val deadline = SystemClock.elapsedRealtime() + 30_000
            while (!outputDone && SystemClock.elapsedRealtime() < deadline) {
                if (!inputDone) {
                    val inputIndex = codec.dequeueInputBuffer(10_000)
                    if (inputIndex >= 0) {
                        val buffer = requireNotNull(codec.getInputBuffer(inputIndex)).apply { clear() }
                        val count = extractor.readSampleData(buffer, 0)
                        if (count < 0) {
                            codec.queueInputBuffer(inputIndex, 0, 0, 0, MediaCodec.BUFFER_FLAG_END_OF_STREAM)
                            inputDone = true
                        } else {
                            codec.queueInputBuffer(inputIndex, 0, count, extractor.sampleTime, 0)
                            extractor.advance()
                        }
                    }
                }
                val outputIndex = codec.dequeueOutputBuffer(info, 10_000)
                if (outputIndex == MediaCodec.INFO_OUTPUT_FORMAT_CHANGED) {
                    assertEquals(48_000, codec.outputFormat.getInteger(MediaFormat.KEY_SAMPLE_RATE))
                    assertEquals(2, codec.outputFormat.getInteger(MediaFormat.KEY_CHANNEL_COUNT))
                    if (codec.outputFormat.containsKey(MediaFormat.KEY_PCM_ENCODING)) {
                        assertEquals(AudioFormat.ENCODING_PCM_16BIT, codec.outputFormat.getInteger(MediaFormat.KEY_PCM_ENCODING))
                    }
                } else if (outputIndex >= 0) {
                    if (info.size > 0) {
                        assertTrue("Decoder exceeded the fixture's bounded PCM budget", output.size() + info.size <= 1_000_000)
                        val buffer = requireNotNull(codec.getOutputBuffer(outputIndex))
                        buffer.position(info.offset); buffer.limit(info.offset + info.size)
                        val bytes = ByteArray(info.size)
                        buffer.get(bytes); output.write(bytes)
                    }
                    outputDone = info.flags and MediaCodec.BUFFER_FLAG_END_OF_STREAM != 0
                    codec.releaseOutputBuffer(outputIndex, false)
                }
            }
            assertTrue("Audio decoding did not reach EOS", outputDone)
            return output.toByteArray()
        } finally {
            runCatching { decoder?.stop() }
            decoder?.release()
            extractor.release()
        }
    }

    private fun verifyTones(pcm: ByteArray) {
        assertTrue("Decoded audio is unexpectedly short or long", pcm.size in 360_000..405_000)
        val samples = ByteBuffer.wrap(pcm).order(ByteOrder.LITTLE_ENDIAN).asShortBuffer()
        val frames = samples.remaining() / 2
        for ((channel, expectedFrequency) in listOf(0 to 440.0, 1 to 660.0)) {
            var positiveCrossings = 0
            var sumSquares = 0.0
            var previous = 0
            for (frame in 0 until frames) {
                val sample = samples.get(frame * 2 + channel).toInt()
                if (previous <= 0 && sample > 0) positiveCrossings += 1
                sumSquares += sample.toDouble() * sample
                previous = sample
            }
            assertTrue("Decoded tone is silent or severely attenuated", sqrt(sumSquares / frames) > 1_000)
            val frequency = positiveCrossings * 48_000.0 / frames
            assertTrue("Channel $channel expected $expectedFrequency Hz but decoded $frequency Hz", abs(frequency - expectedFrequency) < 12)
        }
    }

    private fun wavPcm(bytes: ByteArray): ByteArray {
        assertEquals("RIFF", String(bytes, 0, 4, Charsets.US_ASCII))
        assertEquals("WAVE", String(bytes, 8, 4, Charsets.US_ASCII))
        var offset = 12
        while (offset + 8 <= bytes.size) {
            val size = ByteBuffer.wrap(bytes, offset + 4, 4).order(ByteOrder.LITTLE_ENDIAN).int
            assertTrue(size >= 0 && offset.toLong() + 8 + size <= bytes.size)
            if (String(bytes, offset, 4, Charsets.US_ASCII) == "data") return bytes.copyOfRange(offset + 8, offset + 8 + size)
            offset += 8 + size + (size and 1)
        }
        error("The independent WAV fixture has no PCM data chunk")
    }

    private fun assertDuration(format: MediaFormat) {
        assertTrue("Output duration is unavailable", format.containsKey(MediaFormat.KEY_DURATION))
        assertTrue("Output duration differs from the independently generated two-second source: $format",
            format.getLong(MediaFormat.KEY_DURATION) in 1_900_000..2_150_000)
    }

    private fun grantTransient(name: String): Uri {
        val result = resolver.call(provider, "grant-fixture", name, null)!!
        return Uri.parse(result.getString("uri")!!)
    }

    private fun grant(name: String): Uri = grantTransient(name).also { uri ->
        resolver.takePersistableUriPermission(uri, android.content.Intent.FLAG_GRANT_READ_URI_PERMISSION)
    }

    private fun revoke(name: String) {
        val uri = DocumentsContract.buildDocumentUri(AUTHORITY, name)
        runCatching { resolver.releasePersistableUriPermission(uri, android.content.Intent.FLAG_GRANT_READ_URI_PERMISSION) }
        resolver.call(provider, "revoke-fixture", name, null)
    }

    private fun hash(uri: Uri): String {
        val digest = MessageDigest.getInstance("SHA-256")
        resolver.openInputStream(uri)!!.use { input ->
            val buffer = ByteArray(64 * 1024)
            while (true) {
                val count = input.read(buffer)
                if (count < 0) break
                digest.update(buffer, 0, count)
            }
        }
        return digest.digest().joinToString("") { "%02x".format(it) }
    }

    private fun awaitJob(id: String): JobSnapshot {
        var last: JobSnapshot? = null
        waitUntil("Conversion did not complete within the bounded test timeout", 120_000) {
            last = media.jobsNative().firstOrNull { it.id == id }
            if (last?.state in setOf("failed", "canceled", "interrupted")) fail("Conversion ended as ${last?.state}: ${last?.error}")
            last?.state == "completed"
        }
        return requireNotNull(last)
    }

    private fun awaitNewJob(previous: Set<String>): JobSnapshot {
        var result: JobSnapshot? = null
        waitUntil("The visible Convert action did not submit a job", 30_000) {
            result = media.jobsNative().firstOrNull { it.id !in previous }
            result != null
        }
        return requireNotNull(result)
    }

    private fun cleanupNewJobs(previous: Set<String>) = media.jobsNative().filter { it.id !in previous }.forEach { cleanupJob(it.id) }

    private fun cleanupJob(id: String) {
        var job = media.jobsNative().firstOrNull { it.id == id } ?: return
        if (job.state !in setOf("completed", "failed", "canceled")) {
            media.cancelNative(id)
            waitUntil("Test job did not stop before cleanup", 30_000) {
                job = media.jobsNative().first { it.id == id }
                job.state in setOf("completed", "failed", "canceled")
            }
        }
        job.outputLocator?.let { resolver.delete(Uri.parse(it), null, null) }
        media.dismissNative(id)
    }

    private fun awaitPicker() {
        // Package visibility can hide an intent handler from resolveActivity
        // even though Android successfully launches it. Observe the real
        // DocumentsUI surface on these emulator images instead of querying it.
        waitUntil("The system document picker did not open", 15_000) {
            val foreground = device.currentPackageName
            foreground?.endsWith(".documentsui") == true && device.hasObject(By.pkg(foreground))
        }
    }

    private fun chooseDocument(name: String) {
        awaitPicker()
        var root = device.wait(Until.findObject(By.text("Velmux Test Sources")), 2_000)
        if (root == null) {
            val drawer = device.findObject(By.desc("Show roots"))
                ?: device.findObject(By.descContains("navigation drawer"))
                ?: error("The system picker has no accessible roots control")
            drawer.click()
            root = device.wait(Until.findObject(By.text("Velmux Test Sources")), 10_000)
        }
        requireNotNull(root) { "The read-only test document provider was not listed by DocumentsUI" }.click()
        // Compact DocumentsUI can expose an action's description behind the
        // overflow button. Choose the visible menu row, then verify the actual
        // one-column layout before searching for the document.
        device.wait(Until.hasObject(By.res("android", "title")), 10_000)
        val moreOptions = device.findObject(By.desc("More options"))
        moreOptions?.click()
        val listOption = device.wait(Until.findObject(By.text("List view")), 3_000)
        if (listOption != null) listOption.click()
        else if (device.hasObject(By.text("Grid view"))) device.pressBack()
        else {
            if (moreOptions != null) device.pressBack()
            device.findObject(By.desc("List view"))?.click()
        }
        device.waitForIdle(2_000)
        waitUntil("DocumentsUI did not apply its list layout", 10_000) {
            val titles = device.findObjects(By.res("android", "title")).filter {
                it.text.substringAfterLast('.', "") in setOf("bin", "mp4", "mkv", "wav", "flac")
            }
            titles.size >= 2 && titles.map { it.visibleBounds.left }.distinct().size == 1
        }
        var document = device.wait(Until.findObject(By.text(name)), 3_000)
        if (document == null) {
            // DocumentsUI sorts these genuine source rows; the requested video
            // can be below the first screen when all fixture formats are listed.
            val list = UiScrollable(UiSelector().resourceIdMatches(".*:id/dir_list")).setAsVerticalList().setMaxSearchSwipes(12)
            runCatching { list.scrollIntoView(UiSelector().text(name)) }
            document = device.wait(Until.findObject(By.text(name)), 3_000)
        }
        if (document == null) {
            capture("failure-picker-source-list.png")
            error("The generated verification source was not visible after scrolling: $name")
        }
        document.click()
        if (!device.wait(Until.hasObject(By.pkg(context.packageName)), 5_000)) {
            device.findObject(By.text(java.util.regex.Pattern.compile("(?i)open")))?.click()
        }
        assertTrue("The picker did not return the selected document", device.wait(Until.hasObject(By.pkg(context.packageName)), 15_000))
    }

    private fun awaitText(text: String, exact: Boolean = false) {
        val selector = if (exact) By.text(text) else By.textContains(text)
        assertTrue("Visible UI text was not found: $text", device.wait(Until.hasObject(selector), 30_000))
    }

    private fun clickText(text: String, exact: Boolean = false) = reachableText(text, exact).click()

    private fun reachableText(text: String, exact: Boolean = false): UiObject2 {
        val selector = if (exact) By.text(text) else By.textContains(text)
        var target = device.wait(Until.findObject(selector), 5_000)
        if (target == null) {
            val scroll = UiScrollable(UiSelector().scrollable(true)).setMaxSearchSwipes(6)
            runCatching { scroll.scrollIntoView(if (exact) UiSelector().text(text) else UiSelector().textContains(text)) }
            target = device.wait(Until.findObject(selector), 5_000)
        }
        if (target == null) {
            // In a short landscape viewport the native player can occupy the
            // entire middle of the scroll area and consume those gestures.
            // Swipe the real shared content gutter instead of its controls.
            for (forward in listOf(true, false)) {
                repeat(12) {
                    if (target == null) {
                        val area = device.findObject(By.scrollable(true))?.visibleBounds
                        if (area != null && area.height() > 32) {
                            val x = area.left + minOf(8, area.width() / 8)
                            val top = area.top + 8
                            val bottom = area.bottom - 8
                            device.swipe(x, if (forward) bottom else top, x, if (forward) top else bottom, 20)
                            target = device.findObject(selector)
                        }
                    }
                }
            }
        }
        return requireNotNull(target) { "The UI content could not be reached by scrolling: $text" }
    }

    private fun awaitRenderedDarkAppearance() {
        waitUntil("The selected dark appearance did not reach the actual Settings UI") {
            device.findObject(By.text("Dark"))?.parent?.isChecked == true
        }
        waitUntil("The Settings surface did not render its dark palette") {
            val screenshot = instrumentation.uiAutomation.takeScreenshot() ?: return@waitUntil false
            try {
                // The right content margin is the shared theme's background,
                // away from labels, controls and system status/navigation bars.
                val pixel = screenshot.getPixel(screenshot.width - 8, screenshot.height / 2)
                abs(Color.red(pixel) - 0x17) <= 2 && abs(Color.green(pixel) - 0x12) <= 2 && abs(Color.blue(pixel) - 0x0e) <= 2
            } finally { screenshot.recycle() }
        }
    }

    private fun capture(name: String) {
        device.waitForIdle(2_000)
        val directory = File(context.filesDir, "screenshots")
        check(directory.isDirectory || directory.mkdirs())
        val screenshot = File(directory, name)
        assertTrue("Could not retain an actual device screenshot", device.takeScreenshot(screenshot))
        retainEvidence(name, screenshot.readBytes())
        val hierarchy = File(directory, name.removeSuffix(".png") + ".xml")
        device.dumpWindowHierarchy(hierarchy)
        retainEvidence(hierarchy.name, hierarchy.readBytes())
    }

    private fun retainOutput(name: String, uri: Uri) {
        val bytes = resolver.openInputStream(uri)!!.use { input ->
            val output = ByteArrayOutputStream()
            val buffer = ByteArray(64 * 1024)
            while (true) {
                val count = input.read(buffer)
                if (count < 0) break
                assertTrue("Verification output exceeded its bounded artifact size", output.size() + count <= 4 * 1024 * 1024)
                output.write(buffer, 0, count)
            }
            output.toByteArray()
        }
        retainEvidence(name, bytes)
    }

    private fun retainText(name: String, text: String) = retainEvidence(name, text.take(100_000).toByteArray(Charsets.UTF_8))

    private fun retainEvidence(name: String, bytes: ByteArray) {
        val key = "api-${Build.VERSION.SDK_INT}-$name"
        for (offset in bytes.indices step 256 * 1024) {
            val chunk = bytes.copyOfRange(offset, minOf(bytes.size, offset + 256 * 1024))
            resolver.call(provider, "write-evidence", key, Bundle().apply {
                putByteArray("bytes", chunk)
                putLong("offset", offset.toLong())
            }) ?: error("The standalone fixture APK did not retain verification evidence")
        }
    }

    private fun waitUntil(message: String, timeout: Long = 30_000, predicate: () -> Boolean) {
        val deadline = SystemClock.elapsedRealtime() + timeout
        while (SystemClock.elapsedRealtime() < deadline) {
            if (predicate()) return
            SystemClock.sleep(50)
        }
        fail(message)
    }

    private companion object {
        const val AUTHORITY = "com.velmux.mobile.test.documents"
        const val PICKER_VIDEO = "video.bin"
    }
}

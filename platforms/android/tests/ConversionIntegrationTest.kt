package com.velmux.mobile

import android.media.AudioFormat
import android.media.MediaCodec
import android.media.MediaExtractor
import android.media.MediaFormat
import android.media.MediaMetadataRetriever
import android.net.Uri
import android.os.SystemClock
import android.provider.DocumentsContract
import androidx.media3.common.MediaItem
import androidx.media3.common.PlaybackException
import androidx.media3.common.Player
import androidx.media3.common.util.UnstableApi
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.ui.PlayerView
import androidx.test.core.app.ActivityScenario
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import androidx.test.uiautomator.By
import androidx.test.uiautomator.UiDevice
import androidx.test.uiautomator.UiScrollable
import androidx.test.uiautomator.UiSelector
import androidx.test.uiautomator.Until
import com.velmux.ui.app.JobSnapshot
import org.junit.Assert.*
import org.junit.Test
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
import kotlin.math.abs
import kotlin.math.sqrt

@RunWith(AndroidJUnit4::class)
class ConversionIntegrationTest {
    private val instrumentation get() = InstrumentationRegistry.getInstrumentation()
    private val context get() = instrumentation.targetContext
    private val media get() = (context.applicationContext as VelmuxApplication).session.media
    private val resolver get() = context.contentResolver
    private val provider = Uri.parse("content://$AUTHORITY")
    private val device get() = UiDevice.getInstance(instrumentation)

    @Test
    fun systemPickerCancelThenVideoConversionPublishesDecodedOutput() {
        media.beginNative()
        val before = media.jobsNative().map { it.id }.toSet()
        val uri = grant("video.mp4")
        val sourceHash = hash(uri)
        revoke("video.mp4")
        assertFalse(resolver.persistedUriPermissions.any { it.uri == uri })
        try {
            ActivityScenario.launch(MainActivity::class.java).use {
                awaitText("Video Converter")
                capture("ui-home.png")
                clickText("Video Converter")
                awaitPicker()
                device.pressBack()
                awaitText("Video Converter")
                assertFalse("Picker cancellation must not become a source error", device.hasObject(By.text("Source unavailable")))
                assertEquals(before, media.jobsNative().map { it.id }.toSet())
                clickText("Video Converter")
                chooseDocument("video.mp4")
                awaitText("video.mp4")
                awaitText("Convert", exact = true)
                assertTrue("The picker result must have durable read permission",
                    resolver.persistedUriPermissions.any { it.uri == uri && it.isReadPermission })
                clickText("Codec")
                clickText("H.264")
                capture("ui-video-workspace.png")
                clickText("Convert", exact = true)
                val submitted = awaitNewJob(before)
                val completed = awaitJob(submitted.id)
                assertEquals("completed", completed.state)
                val output = Uri.parse(requireNotNull(completed.outputLocator))
                verifyVideo(output)
                assertEquals(sourceHash, hash(uri))
                awaitText("Completed", exact = true)
                capture("ui-jobs.png")
                clickText("Back", exact = true)
                clickText("Settings", exact = true)
                clickText("Dark", exact = true)
                waitUntil("Appearance was not committed") { media.appearanceNative() == "dark" }
                capture("ui-settings-dark.png")
            }
            val accepted = media.jobsNative().filter { it.id !in before }
            assertEquals("One Convert action must produce one accepted job", 1, accepted.size)
        } finally {
            runCatching { media.setAppearanceNative("system") }
            cleanupNewJobs(before)
            revoke("video.mp4")
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
                assertEquals(expectedMime, formats.single().getString(MediaFormat.KEY_MIME))
                assertEquals(48_000, formats.single().getInteger(MediaFormat.KEY_SAMPLE_RATE))
                assertEquals(2, formats.single().getInteger(MediaFormat.KEY_CHANNEL_COUNT))
                assertDuration(formats.single())
                val decoded = decodeAudio(output)
                verifyTones(decoded)
                if (format == OutputFormat.FLAC) {
                    val originalPcm = instrumentation.context.assets.open("stereo.wav").use { wavPcm(it.readBytes()) }
                    assertArrayEquals("Lossless conversion changed the known source PCM samples", originalPcm, decoded)
                }
                assertEquals(1, media.jobsNative().count { it.id == accepted.id })
            }
            assertEquals(beforeHash, hash(uri))
        } finally {
            media.releaseNative(inspected.id)
            acceptedIds.forEach { cleanupJob(it) }
            revoke("stereo.wav")
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

    private fun grant(name: String): Uri {
        val result = resolver.call(provider, "grant-fixture", name, null)!!
        val uri = Uri.parse(result.getString("uri")!!)
        resolver.takePersistableUriPermission(uri, android.content.Intent.FLAG_GRANT_READ_URI_PERMISSION)
        return uri
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
        val pickerPackage = context.packageManager.resolveActivity(AndroidPlatformActions.selectionIntent(true), 0)?.activityInfo?.packageName
            ?: error("No system document picker is installed")
        assertTrue("The system document picker did not open", device.wait(Until.hasObject(By.pkg(pickerPackage)), 15_000))
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
        val document = device.wait(Until.findObject(By.text(name)), 10_000)
            ?: error("The generated verification source was not visible: $name")
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

    private fun clickText(text: String, exact: Boolean = false) {
        val selector = if (exact) By.text(text) else By.textContains(text)
        var target = device.wait(Until.findObject(selector), 5_000)
        if (target == null) {
            val scroll = UiScrollable(UiSelector().scrollable(true)).setMaxSearchSwipes(6)
            runCatching { scroll.scrollIntoView(if (exact) UiSelector().text(text) else UiSelector().textContains(text)) }
            target = device.wait(Until.findObject(selector), 5_000)
        }
        requireNotNull(target) { "The visible UI action could not be found: $text" }.click()
    }

    private fun capture(name: String) {
        device.waitForIdle(2_000)
        val directory = File(requireNotNull(context.getExternalFilesDir(null)), "screenshots")
        check(directory.isDirectory || directory.mkdirs())
        assertTrue("Could not retain an actual device screenshot", device.takeScreenshot(File(directory, name)))
    }

    private fun waitUntil(message: String, timeout: Long = 30_000, predicate: () -> Boolean) {
        val deadline = SystemClock.elapsedRealtime() + timeout
        while (SystemClock.elapsedRealtime() < deadline) {
            if (predicate()) return
            SystemClock.sleep(50)
        }
        fail(message)
    }

    private companion object { const val AUTHORITY = "com.velmux.mobile.test.documents" }
}

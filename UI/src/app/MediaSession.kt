package com.velmux.ui.app

import kotlinx.atomicfu.atomic
import kotlinx.atomicfu.locks.SynchronizedObject
import kotlinx.atomicfu.locks.synchronized
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.Runnable
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withContext
import uniffi.velmux_core.CancellationToken
import uniffi.velmux_core.ConversionConfig
import uniffi.velmux_core.Engine
import uniffi.velmux_core.EngineException
import uniffi.velmux_core.FormatOption
import uniffi.velmux_core.JobObserver
import uniffi.velmux_core.JobRecord
import uniffi.velmux_core.MediaInfo
import uniffi.velmux_core.MediaProvider
import uniffi.velmux_core.OpenedSource
import uniffi.velmux_core.Operation
import uniffi.velmux_core.OutputReservation
import uniffi.velmux_core.PublishedOutput
import uniffi.velmux_core.PreviewResult
import uniffi.velmux_core.StreamKind
import kotlin.coroutines.resumeWithException

/** Presentation facts retain unknown values and the authoritative inspected media record. */
data class SourceSnapshot(
    val id: String,
    val locator: String,
    val name: String,
    val summary: String,
    val durationUs: Long?,
    val byteSize: Long?,
    val info: MediaInfo,
)

/** Signed primitive fields keep native verification independent of Kotlin unsigned export details. */
data class JobSnapshot(
    val id: String,
    val sourceName: String,
    val fileName: String,
    val state: String,
    val processedUs: Long,
    val durationUs: Long?,
    val error: String?,
    val outputLocator: String?,
    val outputName: String?,
    val outputSize: Long?,
)

/** Owns only a transient request token. Accepted job execution is owned independently by Rust. */
class MediaRequest internal constructor(private val onReleased: (MediaRequest) -> Unit) {
    private val gate = SynchronizedObject()
    private val token = CancellationToken()
    private var users = 0
    private var closing = false
    private var released = false

    fun cancel() = synchronized(gate) {
        if (!released) token.cancel()
    }

    fun close() {
        val detached = synchronized(gate) {
            if (!closing) {
                closing = true
                token.cancel()
            }
            releaseIfIdle()
        }
        if (detached) onReleased(this)
    }

    internal fun <T> call(action: (CancellationToken) -> T): T {
        synchronized(gate) {
            if (closing) throw EngineException.Cancelled()
            users += 1
        }
        try {
            return action(token)
        } finally {
            val detached = synchronized(gate) {
                users -= 1
                releaseIfIdle()
            }
            if (detached) onReleased(this)
        }
    }

    private fun releaseIfIdle(): Boolean {
        if (!closing || released || users != 0) return false
        released = true
        token.close()
        return true
    }
}

/** Borrows the application-owned engine; it never creates or closes another engine. */
class MediaSession(private val engine: Engine) {
    private val startupGate = SynchronizedObject()
    private val requestGate = SynchronizedObject()
    private val closed = atomic(false)
    private var services: MediaServices? = null
    private var initialized = false
    private var observing = false
    private val requests = mutableSetOf<MediaRequest>()
    private val mutableJobsRevision = MutableStateFlow(0L)
    val jobsRevision: StateFlow<Long> = mutableJobsRevision.asStateFlow()

    private val observer = object : JobObserver {
        override fun onJobsChanged() {
            if (!closed.value) mutableJobsRevision.update { it + 1 }
        }
    }

    @Throws(IllegalStateException::class)
    fun attach(services: MediaServices) = synchronized(startupGate) {
        checkOpen()
        check(this.services == null || this.services === services) {
            "Native media services are already attached to this application session."
        }
        this.services = services
    }

    @Throws(IllegalStateException::class)
    fun beginRequest(): MediaRequest = synchronized(requestGate) {
        checkOpen()
        MediaRequest { request -> synchronized(requestGate) { requests.remove(request) } }
            .also { requests.add(it) }
    }

    @Throws(EngineException::class, IllegalStateException::class)
    suspend fun ensureStarted() = withContext(Dispatchers.Default) { beginNative() }

    /** Synchronous bridge for native tests; invoke on a worker thread. */
    @Throws(EngineException::class, IllegalStateException::class)
    fun beginNative() = guarded { start() }

    private fun start() = synchronized(startupGate) {
        checkOpen()
        val native = checkNotNull(services) { "Native media services have not been attached." }
        if (!initialized) {
            engine.initializeMedia(NativeMediaAdapter(native))
            initialized = true
        }
        if (!observing) {
            engine.observeJobs(observer)
            observing = true
        }
    }

    @Throws(EngineException::class, IllegalStateException::class)
    suspend fun inspect(selected: SelectedSource, video: Boolean, request: MediaRequest): SourceSnapshot =
        requestCall(request, onAbandoned = { source ->
            // A canceled continuation must not strand an inspection that finished concurrently.
            runCatching { if (!closed.value) engine.releaseInspection(source.id) }
        }) { token -> inspectReady(selected.locator, selected.displayName, video, token) }

    @Throws(EngineException::class, IllegalStateException::class)
    fun inspectNative(locator: String, name: String, video: Boolean): SourceSnapshot = ownRequest { request ->
        request.call { token -> ready { inspectReady(locator, name, video, token) } }
    }

    @Throws(EngineException::class, IllegalStateException::class)
    fun inspectWithRequestNative(locator: String, name: String, video: Boolean, request: MediaRequest): SourceSnapshot =
        request.call { token -> ready { inspectReady(locator, name, video, token) } }

    private fun inspectReady(locator: String, name: String, video: Boolean, token: CancellationToken): SourceSnapshot {
        val source = engine.inspectSource(locator, name, operation(video), token)
        val primary = source.info.streams.firstOrNull {
            if (video) it.kind == StreamKind.VIDEO && it.video?.attachedPicture != true
            else it.kind == StreamKind.AUDIO
        }
        val geometry = primary?.video?.let { info ->
            if (info.width != null && info.height != null) "${info.width} × ${info.height}" else null
        }
        val audio = primary?.audio?.let { info ->
            listOfNotNull(info.sampleRate?.let { "$it Hz" }, info.channelLayout).joinToString(" · ").ifBlank { null }
        }
        val container = when {
            source.info.container.split(',').any { it == "mov" || it == "mp4" } -> "MP4 / QuickTime"
            source.info.container.split(',').contains("matroska") -> "Matroska / WebM"
            else -> source.info.container.uppercase()
        }
        val orientation = primary?.video?.rotationDegreesCounterClockwise?.takeIf { it != 0.0 }?.let { "Rotation ${it.toInt()}°" }
        return SourceSnapshot(
            id = source.id,
            locator = source.locator,
            name = source.displayName,
            summary = listOfNotNull(container, primary?.codec, geometry, orientation, audio).joinToString(" · "),
            durationUs = source.info.durationUs,
            byteSize = source.info.byteSize.signedOrNull(),
            info = source.info,
        )
    }

    @Throws(EngineException::class, IllegalStateException::class)
    suspend fun preview(id: String, startUs: Long, request: MediaRequest): PreviewResult =
        requestCall(request) { token -> engine.preparePreview(id, startUs, token) }

    @Throws(EngineException::class, IllegalStateException::class)
    fun previewNative(id: String, startUs: Long): PreviewResult = ownRequest { request ->
        request.call { token -> ready { engine.preparePreview(id, startUs, token) } }
    }

    /** Inspection cleanup survives cancellation of the workspace coroutine that owned it. */
    @Throws(EngineException::class, IllegalStateException::class)
    suspend fun release(id: String) = withContext(NonCancellable + Dispatchers.Default) {
        if (!closed.value) releaseNative(id)
    }

    @Throws(EngineException::class, IllegalStateException::class)
    fun releaseNative(id: String) = ready { engine.releaseInspection(id) }

    /** Survives composition disposal without running native grant I/O on main. */
    fun releaseAfterDisposal(id: String) {
        Dispatchers.Default.dispatch(kotlin.coroutines.EmptyCoroutineContext, Runnable {
            runCatching { if (!closed.value) engine.releaseInspection(id) }
        })
    }

    @Throws(EngineException::class, IllegalStateException::class)
    suspend fun defaults(id: String, video: Boolean): ConversionConfig = withContext(Dispatchers.Default) {
        defaultsNative(id, video)
    }

    @Throws(EngineException::class, IllegalStateException::class)
    fun defaultsNative(id: String, video: Boolean): ConversionConfig = ready {
        engine.defaultConfiguration(id, operation(video))
    }

    /** Explicit encoding intent for native callers; final validation remains in Rust. */
    @Throws(EngineException::class, IllegalStateException::class)
    fun encodingConfigurationNative(id: String, video: Boolean, fileName: String): ConversionConfig {
        val defaults = defaultsNative(id, video)
        return defaults.copy(
            format = if (video) uniffi.velmux_core.OutputFormat.MP4 else uniffi.velmux_core.OutputFormat.M4A,
            videoCodec = if (video) uniffi.velmux_core.VideoCodec.H264 else uniffi.velmux_core.VideoCodec.AUTOMATIC,
            audioCodec = if (video) uniffi.velmux_core.AudioCodec.AUTOMATIC else uniffi.velmux_core.AudioCodec.AAC,
            fileName = fileName,
        )
    }

    /** The core catalog is small, deterministic and independent of source probing. */
    @Throws(EngineException::class, IllegalStateException::class)
    fun catalog(video: Boolean): List<FormatOption> = guarded {
        checkOpen()
        engine.mediaCatalog(operation(video))
    }
    @Throws(EngineException::class, IllegalStateException::class)
    fun catalogForSource(id: String, video: Boolean): List<FormatOption> = ready { engine.mediaCatalogForSource(id, operation(video)) }

    @Throws(EngineException::class, IllegalStateException::class)
    fun configurationChoices(id: String, configuration: ConversionConfig) = ready { engine.configurationChoices(id, configuration) }

    @Throws(EngineException::class, IllegalStateException::class)
    suspend fun validate(id: String, configuration: ConversionConfig) = withContext(Dispatchers.Default) {
        validateNative(id, configuration)
    }

    @Throws(EngineException::class, IllegalStateException::class)
    fun validateNative(id: String, configuration: ConversionConfig) = ready {
        engine.validateConfiguration(id, configuration)
    }

    @Throws(EngineException::class, IllegalStateException::class)
    suspend fun submit(id: String, key: String, configuration: ConversionConfig, request: MediaRequest): JobRecord =
        requestCall(request) { token -> engine.submitConversion(id, key, configuration, token) }

    @Throws(EngineException::class, IllegalStateException::class)
    fun submitNative(id: String, key: String, configuration: ConversionConfig): JobSnapshot = ownRequest { request ->
        request.call { token -> ready { engine.submitConversion(id, key, configuration, token).snapshot() } }
    }

    @Throws(EngineException::class, IllegalStateException::class)
    suspend fun jobs(): List<JobRecord> = withContext(Dispatchers.Default) { ready { engine.jobs() } }

    @Throws(EngineException::class, IllegalStateException::class)
    fun jobsNative(): List<JobSnapshot> = ready { engine.jobs().map { it.snapshot() } }

    @Throws(EngineException::class, IllegalStateException::class)
    suspend fun cancel(id: String) = withContext(Dispatchers.Default) { cancelNative(id) }

    @Throws(EngineException::class, IllegalStateException::class)
    fun cancelNative(id: String) = ready { engine.cancelJob(id) }

    @Throws(EngineException::class, IllegalStateException::class)
    suspend fun restart(id: String) = withContext(Dispatchers.Default) { restartNative(id) }

    @Throws(EngineException::class, IllegalStateException::class)
    fun restartNative(id: String) = ready { engine.restartJob(id) }

    @Throws(EngineException::class, IllegalStateException::class)
    suspend fun retryPublication(id: String) = withContext(Dispatchers.Default) { retryPublicationNative(id) }

    @Throws(EngineException::class, IllegalStateException::class)
    fun retryPublicationNative(id: String) = ready { engine.retryPublication(id) }

    @Throws(EngineException::class, IllegalStateException::class)
    suspend fun dismiss(id: String) = withContext(Dispatchers.Default) { dismissNative(id) }

    @Throws(EngineException::class, IllegalStateException::class)
    fun dismissNative(id: String) = ready { engine.dismissJob(id) }

    @Throws(EngineException::class, IllegalStateException::class)
    suspend fun appearance(): String = withContext(Dispatchers.Default) { appearanceNative() }

    @Throws(EngineException::class, IllegalStateException::class)
    fun appearanceNative(): String = ready { engine.appearance() }

    @Throws(EngineException::class, IllegalStateException::class)
    suspend fun setAppearance(value: String) = withContext(Dispatchers.Default) { setAppearanceNative(value) }

    @Throws(EngineException::class, IllegalStateException::class)
    fun setAppearanceNative(value: String) = ready { engine.setAppearance(value) }

    @Throws(EngineException::class, IllegalStateException::class)
    suspend fun outputLocation(): String = withContext(Dispatchers.Default) { outputLocationNative() }

    @Throws(EngineException::class, IllegalStateException::class)
    fun outputLocationNative(): String = ready { engine.outputLocation() }

    @Throws(EngineException::class, IllegalStateException::class)
    suspend fun cacheBytes(): Long = withContext(Dispatchers.Default) { cacheBytesNative() }

    @Throws(EngineException::class, IllegalStateException::class)
    fun cacheBytesNative(): Long = ready {
        val bytes = engine.cacheBytes()
        if (bytes > Long.MAX_VALUE.toULong()) throw EngineException.Storage("Cache usage exceeds the native size reporting limit.")
        bytes.toLong()
    }

    @Throws(EngineException::class, IllegalStateException::class)
    suspend fun clearCache() = withContext(Dispatchers.Default) { clearCacheNative() }

    @Throws(EngineException::class, IllegalStateException::class)
    fun clearCacheNative() = ready { engine.clearCache() }

    @Throws(EngineException::class, IllegalStateException::class)
    suspend fun runtimeError(): String? = withContext(Dispatchers.Default) { ready { engine.runtimeError() } }

    @Throws(EngineException::class, IllegalStateException::class)
    fun runtimeErrorNative(): String? = ready { engine.runtimeError() }

    /** Cancel transient calls, waiting only for initialization, not serializing all engine work. */
    fun close() {
        if (!closed.compareAndSet(expect = false, update = true)) return
        val owned = synchronized(requestGate) { requests.toList() }
        owned.forEach { it.close() }
        synchronized(startupGate) { /* Initialization must finish before the owner closes its engine. */ }
    }

    private fun checkOpen() = check(!closed.value) { "The media session is closed." }

    private fun operation(video: Boolean) = if (video) Operation.VIDEO_CONVERTER else Operation.AUDIO_CONVERTER

    private fun <T> ready(action: () -> T): T = guarded {
        start()
        checkOpen()
        action()
    }

    private inline fun <T> guarded(action: () -> T): T = try {
        action()
    } catch (error: EngineException) {
        throw error
    } catch (error: IllegalStateException) {
        throw error
    } catch (error: Throwable) {
        throw EngineException.Internal(error.boundedMessage("The media engine encountered an unexpected failure."))
    }

    private fun <T> ownRequest(action: (MediaRequest) -> T): T {
        val request = beginRequest()
        try {
            return action(request)
        } finally {
            request.close()
        }
    }

    private suspend fun <T> requestCall(
        request: MediaRequest,
        onAbandoned: (T) -> Unit = {},
        action: (CancellationToken) -> T,
    ): T = suspendCancellableCoroutine { continuation ->
        continuation.invokeOnCancellation { request.cancel() }
        // Resume directly into the caller's dispatcher so prompt cancellation at
        // the UI boundary also runs the abandoned-result cleanup handler.
        Dispatchers.Default.dispatch(continuation.context, Runnable {
            try {
                val result = request.call { token -> ready { action(token) } }
                continuation.resume(result) { _, abandoned, _ -> onAbandoned(abandoned) }
            } catch (error: Throwable) {
                continuation.resumeWithException(error)
            }
        })
    }

    private fun JobRecord.snapshot() = JobSnapshot(
        id, sourceName, fileName, state.name.lowercase(), processedUs, durationUs, error,
        output?.locator, output?.displayName, output?.size.signedOrNull(),
    )
}

private fun ULong?.signedOrNull(): Long? = this?.takeIf { it <= Long.MAX_VALUE.toULong() }?.toLong()

private fun Throwable.boundedMessage(fallback: String): String = message?.take(300)?.ifBlank { null } ?: fallback

/** Native exceptions are translated before returning through a fallible generated callback. */
private class NativeMediaAdapter(private val native: MediaServices) : MediaProvider {
    override fun cacheRoot() = native.cacheRoot()
    override fun outputLocation() = native.outputLocation()
    override fun retainSourceAccess(locator: String) = sourceCall { native.retainSourceAccess(locator) }
    override fun releaseSourceAccess(locator: String) { runCatching { native.releaseSourceAccess(locator) } }
    override fun reconcileSourceAccess(retainedLocators: List<String>) { runCatching { native.reconcileSourceAccess(retainedLocators) } }

    override fun openSource(locator: String): OpenedSource = sourceCall {
        val source = native.openSource(locator)
        // Preserve ownership transfer even for invalid signed ranges: Rust rejects
        // out-of-range values, closes the fd, then acknowledges the native lease.
        OpenedSource(source.fd, source.offset.toULong(), if (source.length == -1L) null else source.length.toULong(), source.leaseId)
    }

    override fun releaseSource(leaseId: String) {
        runCatching { native.releaseSource(leaseId) }
    }

    override fun reserveOutput(jobId: String, fileName: String, mimeType: String): OutputReservation = storageCall {
        native.reserveOutput(jobId, fileName, mimeType).let { OutputReservation(it.locator, it.displayName, it.mimeType) }
    }

    override fun publishOutput(reservation: OutputReservation, workingPath: String, expectedSize: ULong): PublishedOutput = storageCall {
        if (expectedSize > Long.MAX_VALUE.toULong()) throw PlatformFailure("The result exceeds the native storage size limit.")
        native.publishOutput(reservation.native(), workingPath, expectedSize.toLong()).published()
    }

    override fun publicationStatus(reservation: OutputReservation): PublishedOutput? = storageCall {
        native.publicationStatus(reservation.native()).output?.published()
    }

    override fun discardReservation(reservation: OutputReservation) {
        runCatching { native.discardReservation(reservation.native()) }
    }

    private fun OutputReservation.native() = NativeReservation(locator, displayName, mimeType)

    private fun NativePublished.published(): PublishedOutput {
        if (size < 0) throw PlatformFailure("The native destination returned an invalid output size.")
        return PublishedOutput(locator, displayName, size.toULong())
    }

    private inline fun <T> sourceCall(action: () -> T): T = try {
        action()
    } catch (error: Throwable) {
        throw EngineException.SourceAccess(error.boundedMessage("The source provider could not open this document."))
    }

    private inline fun <T> storageCall(action: () -> T): T = try {
        action()
    } catch (error: Throwable) {
        throw EngineException.Storage(error.boundedMessage("The native destination could not complete the storage operation."))
    }
}

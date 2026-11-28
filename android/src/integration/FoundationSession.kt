package com.velmux.ui.app

import kotlinx.atomicfu.atomic
import kotlinx.atomicfu.locks.SynchronizedObject
import kotlinx.atomicfu.locks.synchronized
import uniffi.velmux_core.CancellationToken
import uniffi.velmux_core.Engine
import uniffi.velmux_core.EngineException
import uniffi.velmux_core.NativeProvider

/** Native hosts implement OS facilities; Rust remains the application authority. */
interface PlatformServices {
    fun storageRoot(): String
    fun platformName(): String
    fun onDiagnostic(sequence: Long)
}

/** Diagnostic values used by the native Android integration checks. */
data class FoundationSnapshot(
    val ffmpegVersion: String,
    val encoderCount: Int,
    val decoderCount: Int,
    val muxerCount: Int,
    val demuxerCount: Int,
    val filterCount: Int,
    val storageRoot: String,
    val sequence: Long,
)

/** Exactly one Rust engine is retained for the lifetime of this application host. */
class FoundationSession(services: PlatformServices) {
    private val gate = SynchronizedObject()
    private val closed = atomic(false)
    private val cancellation = CancellationToken()
    private val engine = Engine(object : NativeProvider {
        override fun storageRoot() = services.storageRoot()
        override fun platformName() = services.platformName()
        override fun onDiagnostic(sequence: ULong) {
            check(sequence <= Long.MAX_VALUE.toULong())
            services.onDiagnostic(sequence.toLong())
        }
    })
    val media = MediaSession(engine)

    /** Call away from the UI thread. Native callbacks run on the calling worker. */
    @Throws(EngineException::class, IllegalStateException::class)
    fun inspect(): FoundationSnapshot = synchronized(gate) {
        check(!closed.value) { "The engine session is closed." }
        val report = engine.diagnostics(cancellation)
        FoundationSnapshot(
            ffmpegVersion = report.ffmpegVersion,
            encoderCount = report.encoderCount.toInt(),
            decoderCount = report.decoderCount.toInt(),
            muxerCount = report.muxerCount.toInt(),
            demuxerCount = report.demuxerCount.toInt(),
            filterCount = report.filterCount.toInt(),
            storageRoot = report.storageRoot,
            sequence = report.sequence.toLong(),
        )
    }

    /** Exercises the generated typed error across FFI for native integration tests. */
    @Throws(EngineException::class, IllegalStateException::class)
    fun verifyCancelledRequest(): Boolean = synchronized(gate) {
        check(!closed.value) { "The engine session is closed." }
        val token = CancellationToken()
        try {
            token.cancel()
            try {
                engine.diagnostics(token)
                false
            } catch (_: EngineException.Cancelled) {
                true
            }
        } finally {
            token.close()
        }
    }

    /** Confirms a native storage-access failure reaches Kotlin as the designated typed error. */
    @Throws(EngineException::class, IllegalStateException::class)
    fun verifyInvalidStorageRequest(): Boolean = synchronized(gate) {
        check(!closed.value) { "The engine session is closed." }
        try {
            engine.diagnostics(cancellation)
            false
        } catch (_: EngineException.InvalidStorageRoot) {
            true
        }
    }

    /** Signal cancellation before waiting for an in-flight call to release its resources. */
    fun close() {
        if (!closed.compareAndSet(expect = false, update = true)) return
        media.close()
        cancellation.cancel()
        synchronized(gate) {
            engine.close()
            cancellation.close()
        }
    }
}

package com.velmux.ui.app

import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier

data class SelectedSource(val locator: String, val displayName: String)
data class NativeSource(val fd: Int, val offset: Long, val length: Long, val leaseId: String)
data class NativeReservation(val locator: String, val displayName: String, val mimeType: String)
data class NativePublished(val locator: String, val displayName: String, val size: Long)
/** A non-null result separates "not published" from NSError failure in Swift/ObjC. */
data class NativePublicationStatus(val output: NativePublished?)

/** Narrow native encoder query; Rust has already selected the requested representation. */
data class NativeVideoRequest(
    val codec: String,
    val width: Int,
    val height: Int,
    val frameRateNumerator: Int,
    val frameRateDenominator: Int,
    val bitRate: Long,
    val pixelFormat: String,
    val bitDepth: Int,
    val hdr: Boolean,
    val colorSpace: String?,
    val colorRange: String?,
    val colorPrimaries: String?,
    val colorTransfer: String?,
)
/** Preparation/classification is diagnostic evidence; no frame has been encoded. */
data class NativeVideoCandidate(
    val identifier: String,
    val osReportsHardware: Boolean?,
    val sessionPrepared: Boolean,
)

class PlatformFailure(message: String) : Exception(message)

/** Native implementations own OS handles, not media rules or job state. */
interface MediaServices {
    fun cacheRoot(): String
    fun outputLocation(): String
    @Throws(PlatformFailure::class)
    fun retainSourceAccess(locator: String)
    fun releaseSourceAccess(locator: String)
    fun reconcileSourceAccess(retainedLocators: List<String>)
    @Throws(PlatformFailure::class)
    fun openSource(locator: String): NativeSource
    fun releaseSource(leaseId: String)
    @Throws(PlatformFailure::class)
    fun reserveOutput(jobId: String, fileName: String, mimeType: String): NativeReservation
    @Throws(PlatformFailure::class)
    fun publishOutput(reservation: NativeReservation, workingPath: String, expectedSize: Long): NativePublished
    @Throws(PlatformFailure::class)
    fun publicationStatus(reservation: NativeReservation): NativePublicationStatus
    fun discardReservation(reservation: NativeReservation)
    @Throws(PlatformFailure::class)
    fun nativeVideoCandidates(request: NativeVideoRequest): List<NativeVideoCandidate> = emptyList()
}

/** Picker cancellation returns null; failures are scoped to the current action. */
interface PlatformActions {
    fun selectSource(video: Boolean, completion: (SelectedSource?, String?) -> Unit)
    fun openOutput(locator: String, completion: (String?) -> Unit)
    fun shareOutput(locator: String, completion: (String?) -> Unit)
}

/** Platform players report unsupported playback so the workspace can request a lazy fallback. */
@Composable
expect fun MediaPreview(locator: String, modifier: Modifier, onUnsupported: () -> Unit)

@Composable
expect fun HandleBack(enabled: Boolean, onBack: () -> Unit)

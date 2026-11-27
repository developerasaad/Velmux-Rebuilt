package com.velmux.ui.app

import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier

data class SelectedSource(val locator: String, val displayName: String)
data class NativeSource(val fd: Int, val offset: Long, val length: Long, val leaseId: String)
data class NativeReservation(val locator: String, val displayName: String, val mimeType: String)
data class NativePublished(val locator: String, val displayName: String, val size: Long)

class PlatformFailure(message: String) : Exception(message)

/** Native implementations own OS handles, not media rules or job state. */
interface MediaServices {
    fun cacheRoot(): String
    fun outputLocation(): String
    @Throws(PlatformFailure::class)
    fun openSource(locator: String): NativeSource
    fun releaseSource(leaseId: String)
    @Throws(PlatformFailure::class)
    fun reserveOutput(jobId: String, fileName: String, mimeType: String): NativeReservation
    @Throws(PlatformFailure::class)
    fun publishOutput(reservation: NativeReservation, workingPath: String, expectedSize: Long): NativePublished
    @Throws(PlatformFailure::class)
    fun publicationStatus(reservation: NativeReservation): NativePublished?
    fun discardReservation(reservation: NativeReservation)
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

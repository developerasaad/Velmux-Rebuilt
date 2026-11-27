package com.velmux.mobile

import android.content.ContentValues
import android.content.Intent
import android.net.Uri
import android.os.ParcelFileDescriptor
import android.provider.MediaStore
import android.system.Os
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import com.velmux.ui.app.NativeReservation
import com.velmux.ui.app.PlatformFailure
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File
import java.util.UUID

@RunWith(AndroidJUnit4::class)
class AndroidMediaServicesTest {
    private val context get() = InstrumentationRegistry.getInstrumentation().targetContext
    private val fixtureAuthority = Uri.parse("content://com.velmux.mobile.test.sources")

    @Test
    fun providerOffsetTransferredFdAndPersistedPermissionSurviveLeaseRelease() {
        val key = UUID.randomUUID().toString()
        val resolver = context.contentResolver
        val result = resolver.call(fixtureAuthority, "create", key, null)!!
        val uri = Uri.parse(result.getString("uri")!!)
        try {
            resolver.takePersistableUriPermission(uri, Intent.FLAG_GRANT_READ_URI_PERMISSION)
            assertTrue(resolver.persistedUriPermissions.any { it.uri == uri && it.isReadPermission })
            val services = AndroidMediaServices(context)
            val opened = services.openSource(uri.toString())
            assertEquals(17L, opened.offset)
            assertEquals(64L, opened.length)
            ParcelFileDescriptor.adoptFd(opened.fd).use { descriptor ->
                val payload = ByteArray(64)
                assertEquals(64, Os.pread(descriptor.fileDescriptor, payload, 0, payload.size, opened.offset))
                assertArrayEquals(ByteArray(64) { it.toByte() }, payload)
            }
            // Reusing an fd number must not let lease cleanup close a new unrelated handle.
            val sentinel = File.createTempFile("source-fd-sentinel-", ".bin", context.cacheDir).apply { writeBytes(byteArrayOf(1)) }
            try {
                ParcelFileDescriptor.open(sentinel, ParcelFileDescriptor.MODE_READ_ONLY).use {
                    services.releaseSource(opened.leaseId)
                    services.releaseSource(opened.leaseId)
                    assertEquals(1L, it.statSize)
                }
            } finally { sentinel.delete() }
            val reopenedServices = AndroidMediaServices(context)
            val reopened = reopenedServices.openSource(uri.toString())
            ParcelFileDescriptor.adoptFd(reopened.fd).close()
            reopenedServices.releaseSource(reopened.leaseId)
            assertTrue(resolver.persistedUriPermissions.any { it.uri == uri && it.isReadPermission })
        } finally {
            runCatching { resolver.releasePersistableUriPermission(uri, Intent.FLAG_GRANT_READ_URI_PERMISSION) }
            resolver.call(fixtureAuthority, "remove", key, null)
        }
        expectFailure { AndroidMediaServices(context).openSource(uri.toString()) }
    }

    @Test
    fun reservationsAndPublicationAreDurableIdempotentAndProtectPublishedFiles() {
        val job = UUID.randomUUID().toString()
        val services = AndroidMediaServices(context)
        val bytes = ByteArray(129 * 1024 + 3) { (it % 251).toByte() }
        val working = File.createTempFile("native-publication-", ".bin", context.filesDir).apply { writeBytes(bytes) }
        val first = services.reserveOutput(job, "publication-${UUID.randomUUID()}.bin", "application/octet-stream")
        try {
            val restarted = AndroidMediaServices(context)
            val second = restarted.reserveOutput(job, "another-name.bin", "application/octet-stream")
            assertEquals(first.locator, second.locator)
            assertNull(restarted.publicationStatus(first).output)
            expectFailure { restarted.publishOutput(first, working.absolutePath, bytes.size.toLong() + 1) }
            assertTrue(working.isFile)
            assertNull(restarted.publicationStatus(first).output)
            val published = restarted.publishOutput(first, working.absolutePath, bytes.size.toLong())
            assertEquals(bytes.size.toLong(), published.size)
            context.contentResolver.openInputStream(Uri.parse(published.locator))!!.use {
                assertArrayEquals(bytes, it.readBytes())
            }
            working.delete()
            assertEquals(published, restarted.publishOutput(first, working.absolutePath, bytes.size.toLong()))
            assertEquals(first.locator, AndroidMediaServices(context).reserveOutput(job, "again.bin", "application/octet-stream").locator)
            restarted.discardReservation(first)
            assertEquals(published, restarted.publicationStatus(first).output)
        } finally {
            working.delete()
            // Explicitly remove this manufactured test output, outside production cleanup policy.
            context.contentResolver.delete(Uri.parse(first.locator), null, null)
        }
    }

    @Test
    fun collisionsAndUnsafeOrForeignReservationsCannotOverwriteOrDelete() {
        val services = AndroidMediaServices(context)
        val name = "collision-${UUID.randomUUID()}.bin"
        val first = services.reserveOutput(UUID.randomUUID().toString(), name, "application/octet-stream")
        val second = services.reserveOutput(UUID.randomUUID().toString(), name, "application/octet-stream")
        val downloads = MediaStore.Downloads.getContentUri(MediaStore.VOLUME_EXTERNAL_PRIMARY)
        val foreignUri = context.contentResolver.insert(downloads, ContentValues().apply {
            put(MediaStore.MediaColumns.DISPLAY_NAME, "unmarked-${UUID.randomUUID()}.bin")
            put(MediaStore.MediaColumns.RELATIVE_PATH, "Download/Velmux/")
            put(MediaStore.MediaColumns.MIME_TYPE, "application/octet-stream")
            put(MediaStore.MediaColumns.IS_PENDING, 1)
        })!!
        try {
            assertNotEquals(first.locator, second.locator)
            assertNotEquals(first.displayName, second.displayName)
            expectFailure { services.reserveOutput(UUID.randomUUID().toString(), "../source.bin", "application/octet-stream") }
            services.discardReservation(NativeReservation(foreignUri.toString(), "unmarked.bin", "application/octet-stream"))
            context.contentResolver.query(foreignUri, arrayOf(MediaStore.MediaColumns._ID), null, null, null)!!.use { assertTrue(it.moveToFirst()) }
            services.discardReservation(first)
            context.contentResolver.query(Uri.parse(first.locator), arrayOf(MediaStore.MediaColumns._ID), null, null, null)!!.use { assertFalse(it.moveToFirst()) }
        } finally {
            context.contentResolver.delete(Uri.parse(first.locator), null, null)
            context.contentResolver.delete(Uri.parse(second.locator), null, null)
            context.contentResolver.delete(foreignUri, null, null)
        }
    }

    @Test
    fun documentPickerRequestsExactlyOneDurableReadableSource() {
        val intent = AndroidPlatformActions.selectionIntent(video = true)
        assertEquals(Intent.ACTION_OPEN_DOCUMENT, intent.action)
        assertEquals("video/*", intent.type)
        assertTrue(intent.hasCategory(Intent.CATEGORY_OPENABLE))
        assertFalse(intent.getBooleanExtra(Intent.EXTRA_ALLOW_MULTIPLE, true))
        assertTrue(intent.flags and Intent.FLAG_GRANT_READ_URI_PERMISSION != 0)
        assertTrue(intent.flags and Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION != 0)
    }

    private fun expectFailure(action: () -> Unit) {
        try {
            action()
            fail("Expected a typed platform failure")
        } catch (_: PlatformFailure) {
            // The error is actionable at the shared boundary.
        }
    }
}

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
    fun onlyOwnedDurableReadGrantsAreReleasedAndReconciled() {
        val resolver = context.contentResolver
        val key = UUID.randomUUID().toString()
        val uri = Uri.parse(resolver.call(fixtureAuthority, "create", key, null)!!.getString("uri")!!)
        val services = AndroidMediaServices(context)
        fun hasRead() = resolver.persistedUriPermissions.any { it.uri == uri && it.isReadPermission }
        try {
            assertFalse(hasRead())
            services.retainSourceAccess(uri.toString())
            assertTrue(hasRead())
            AndroidMediaServices(context).reconcileSourceAccess(listOf(uri.toString()))
            assertTrue("Rust-retained source access must survive reconciliation", hasRead())
            val opened = services.openSource(uri.toString())
            ParcelFileDescriptor.adoptFd(opened.fd).close()
            services.releaseSource(opened.leaseId)
            assertTrue("Closing a worker descriptor must not release a durable source reference", hasRead())
            services.releaseSourceAccess(uri.toString())
            assertFalse("Unused owned permission was retained", hasRead())
            // Android clears the persistable offer as well as the retained
            // permission. A new selection must deliver a new real OS grant.
            resolver.call(fixtureAuthority, "create", key, null)
            services.retainSourceAccess(uri.toString())
            AndroidMediaServices(context).reconcileSourceAccess(emptyList())
            assertFalse("Orphaned owned permission survived startup reconciliation", hasRead())
            // An unrelated facility's pre-existing permission never becomes ours.
            resolver.call(fixtureAuthority, "create", key, null)
            resolver.takePersistableUriPermission(uri, Intent.FLAG_GRANT_READ_URI_PERMISSION)
            services.retainSourceAccess(uri.toString())
            services.releaseSourceAccess(uri.toString())
            services.reconcileSourceAccess(emptyList())
            assertTrue("Cleanup released an unowned persisted permission", hasRead())
            resolver.openInputStream(uri)!!.use { assertTrue(it.read() >= 0) }
        } finally {
            runCatching { resolver.releasePersistableUriPermission(uri, Intent.FLAG_GRANT_READ_URI_PERMISSION) }
            resolver.call(fixtureAuthority, "remove", key, null)
        }
    }

    @Test
    fun pickerDeliveryCannotLoseItsPersistableOfferToAnOldSourceRelease() {
        val resolver = context.contentResolver
        val key = UUID.randomUUID().toString()
        val uri = Uri.parse(resolver.call(fixtureAuthority, "create", key, null)!!.getString("uri")!!)
        val locator = uri.toString()
        val services = AndroidMediaServices(context)
        fun hasRead() = resolver.persistedUriPermissions.any { it.uri == uri && it.isReadPermission }
        var activeSelection: Long? = null
        try {
            services.retainSourceAccess(locator)
            val first = AndroidDocumentGrants.beginSelection(context).also { activeSelection = it }
            // Manufacture the same real OS grant that a second picker result
            // delivers while the old Rust source reference is still alive.
            resolver.call(fixtureAuthority, "create", key, null)
            AndroidDocumentGrants.finishSelection(context, first, locator)
            activeSelection = null
            services.releaseSourceAccess(locator)
            assertTrue("Old source cleanup destroyed the new picker handoff", hasRead())
            services.retainSourceAccess(locator)
            val opened = services.openSource(locator)
            ParcelFileDescriptor.adoptFd(opened.fd).close()
            services.releaseSource(opened.leaseId)
            assertTrue("The new source lost lasting access", hasRead())
            services.releaseSourceAccess(locator)
            assertFalse("Claimed handoff prevented final cleanup", hasRead())

            resolver.call(fixtureAuthority, "create", key, null)
            services.retainSourceAccess(locator)
            val canceled = AndroidDocumentGrants.beginSelection(context).also { activeSelection = it }
            services.releaseSourceAccess(locator)
            assertTrue("Cleanup raced an active system picker", hasRead())
            AndroidDocumentGrants.finishSelection(context, canceled, null)
            activeSelection = null
            assertFalse("Canceled selection retained an unused permission", hasRead())

            resolver.call(fixtureAuthority, "create", key, null)
            services.retainSourceAccess(locator)
            val abandoned = AndroidDocumentGrants.beginSelection(context).also { activeSelection = it }
            AndroidDocumentGrants.finishSelection(context, abandoned, locator)
            activeSelection = null
            services.releaseSourceAccess(locator)
            assertTrue(hasRead())
            val next = AndroidDocumentGrants.beginSelection(context).also { activeSelection = it }
            assertFalse("The next picker did not expire an unclaimed handoff", hasRead())
            AndroidDocumentGrants.finishSelection(context, abandoned, locator)
            AndroidDocumentGrants.finishSelection(context, next, null)
            activeSelection = null
        } finally {
            activeSelection?.let { AndroidDocumentGrants.finishSelection(context, it, null) }
            services.reconcileSourceAccess(emptyList())
            runCatching { resolver.releasePersistableUriPermission(uri, Intent.FLAG_GRANT_READ_URI_PERMISSION) }
            resolver.call(fixtureAuthority, "remove", key, null)
        }
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
        assertEquals("*/*", intent.type)
        assertEquals("Select a video source", intent.getStringExtra(Intent.EXTRA_TITLE))
        val audioIntent = AndroidPlatformActions.selectionIntent(video = false)
        assertEquals("*/*", audioIntent.type)
        assertEquals("Select an audio source", audioIntent.getStringExtra(Intent.EXTRA_TITLE))
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

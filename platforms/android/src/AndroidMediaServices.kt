package com.velmux.mobile

import android.content.ContentUris
import android.content.ContentValues
import android.content.Context
import android.content.res.AssetFileDescriptor
import android.net.Uri
import android.os.Environment
import android.os.ParcelFileDescriptor
import android.provider.MediaStore
import com.velmux.ui.app.MediaServices
import com.velmux.ui.app.NativePublished
import com.velmux.ui.app.NativeReservation
import com.velmux.ui.app.NativeSource
import com.velmux.ui.app.PlatformFailure
import java.io.File
import java.io.FileInputStream
import java.security.MessageDigest
import java.util.UUID
import java.util.concurrent.ConcurrentHashMap

/** OS handles and publication primitives. Durable job ownership remains in Rust. */
class AndroidMediaServices(context: Context) : MediaServices {
    private val context = context.applicationContext
    private val resolver = this.context.contentResolver
    private val leases = ConcurrentHashMap<String, AssetFileDescriptor>()
    private val downloads = MediaStore.Downloads.getContentUri(MediaStore.VOLUME_EXTERNAL_PRIMARY)

    override fun cacheRoot(): String = context.cacheDir.absolutePath
    override fun outputLocation(): String = "${Environment.DIRECTORY_DOWNLOADS}/Velmux/"

    override fun openSource(locator: String): NativeSource = platformOperation("The selected file cannot be opened. Select it again if access was removed.") {
        val uri = Uri.parse(locator)
        require(uri.scheme == "content") { "Select a document using the system picker." }
        val descriptor = resolver.openAssetFileDescriptor(uri, "r")
            ?: throw PlatformFailure("The document provider did not return a readable file.")
        var transferredFd = -1
        try {
            val offset = descriptor.startOffset
            val length = descriptor.declaredLength
            require(offset >= 0 && length >= AssetFileDescriptor.UNKNOWN_LENGTH &&
                (length < 0 || offset <= Long.MAX_VALUE - length)) { "The provider returned an invalid file range." }
            val lease = UUID.randomUUID().toString()
            transferredFd = descriptor.parcelFileDescriptor.detachFd()
            leases[lease] = descriptor
            NativeSource(transferredFd, offset, length, lease)
        } catch (error: Exception) {
            if (transferredFd >= 0) ParcelFileDescriptor.adoptFd(transferredFd).close()
            descriptor.close()
            throw error
        }
    }

    /** Rust has already closed the transferred fd; this only releases the provider wrapper. */
    override fun releaseSource(leaseId: String) {
        leases.remove(leaseId)?.runCatching { close() }
    }

    override fun reserveOutput(jobId: String, fileName: String, mimeType: String): NativeReservation =
        platformOperation("The output destination could not be reserved. Check available storage.") {
            synchronized(publicationLock) {
                validateName(fileName)
                require(mimeType.matches(Regex("[A-Za-z0-9!#&^_.+-]+/[A-Za-z0-9!#&^_.+-]+"))) { "The output media type is invalid." }
                require(jobId.isNotBlank() && jobId.length <= 256) { "The output reservation identity is invalid." }
                val marker = resourceMarker(jobId)
                findReservation(marker)?.let {
                    require(it.mime == mimeType) { "The existing reservation belongs to a different output representation." }
                    return@synchronized it.reservation()
                }
                val values = ContentValues().apply {
                    put(MediaStore.MediaColumns.DISPLAY_NAME, fileName)
                    put(MediaStore.MediaColumns.MIME_TYPE, mimeType)
                    put(MediaStore.MediaColumns.RELATIVE_PATH, outputLocation())
                    put(MediaStore.MediaColumns.IS_PENDING, 1)
                    // The generated resource identity lets a retry find the same OS reservation.
                    put(MediaStore.Downloads.DOWNLOAD_URI, marker)
                }
                val uri = resolver.insert(downloads, values)
                    ?: throw PlatformFailure("Shared storage did not create an output destination.")
                val row = readRow(uri) ?: throw PlatformFailure("The reserved output could not be inspected.")
                requireOwned(row)
                row.reservation()
            }
        }

    override fun publishOutput(reservation: NativeReservation, workingPath: String, expectedSize: Long): NativePublished =
        platformOperation("The processed result could not be saved. Its working file has been retained for retry.") {
            synchronized(publicationLock) {
                val uri = reservationUri(reservation.locator)
                val row = readRow(uri) ?: throw PlatformFailure("The output reservation is no longer available.")
                requireOwned(row)
                if (!row.pending) {
                    val existing = published(row)
                    if (existing.size != expectedSize) throw PlatformFailure("The published output does not match the verified result size.")
                    return@synchronized existing
                }
                require(expectedSize > 0) { "The verified result is empty." }
                val working = privateWorkingFile(workingPath)
                require(working.isFile && working.length() == expectedSize) { "The working result has changed or is unavailable." }
                val outputDescriptor = resolver.openFileDescriptor(uri, "rwt")
                    ?: throw PlatformFailure("The reserved destination cannot be written.")
                ParcelFileDescriptor.AutoCloseOutputStream(outputDescriptor).use { output ->
                    FileInputStream(working).use { input ->
                        val buffer = ByteArray(128 * 1024)
                        var copied = 0L
                        while (true) {
                            val count = input.read(buffer)
                            if (count < 0) break
                            if (copied > expectedSize - count) throw PlatformFailure("The working result changed while it was being saved.")
                            output.write(buffer, 0, count)
                            copied += count
                        }
                        if (copied != expectedSize || input.channel.size() != expectedSize) {
                            throw PlatformFailure("The working result changed while it was being saved.")
                        }
                    }
                    output.flush()
                    output.fd.sync()
                }
                if (actualSize(uri) != expectedSize) throw PlatformFailure("Shared storage did not retain the complete result.")
                val revealed = resolver.update(uri, ContentValues().apply {
                    put(MediaStore.MediaColumns.IS_PENDING, 0)
                }, null, null)
                if (revealed != 1) throw PlatformFailure("The result is saved but shared storage did not publish it. Retry saving.")
                val finalRow = readRow(uri) ?: throw PlatformFailure("The published result could not be inspected.")
                if (finalRow.pending) throw PlatformFailure("Shared storage has not published the result yet.")
                val result = published(finalRow)
                if (result.size != expectedSize) throw PlatformFailure("The published output size differs from the verified result.")
                result
            }
        }

    override fun publicationStatus(reservation: NativeReservation): NativePublished? =
        platformOperation("The saved output could not be inspected. Check storage access and retry.") {
            synchronized(publicationLock) {
                val row = readRow(reservationUri(reservation.locator)) ?: return@synchronized null
                requireOwned(row)
                if (row.pending) null else published(row)
            }
        }

    override fun discardReservation(reservation: NativeReservation) {
        // A failure leaves unpublished scratch for later reconciliation. Never delete published media.
        runCatching {
            synchronized(publicationLock) {
                val uri = reservationUri(reservation.locator)
                val row = readRow(uri) ?: return@synchronized
                requireOwned(row)
                if (row.pending) {
                    resolver.delete(uri, "${MediaStore.MediaColumns.IS_PENDING} = ?", arrayOf("1"))
                }
            }
        }
    }

    private fun privateWorkingFile(path: String): File {
        val file = File(path).canonicalFile
        val roots = listOf(context.filesDir.canonicalFile, context.cacheDir.canonicalFile)
        require(roots.any { file.path.startsWith(it.path + File.separator) }) { "The working result is outside application-owned storage." }
        return file
    }

    private fun validateName(name: String) {
        require(name.isNotBlank() && name == name.trim() && name != "." && name != ".." &&
            name.toByteArray(Charsets.UTF_8).size <= 240 && !name.endsWith('.') &&
            name.none { it.code < 32 || it.code == 127 || it in "/\\:*?\"<>|" }) {
            "Choose a file name without path separators or reserved characters."
        }
    }

    private fun resourceMarker(jobId: String): String = RESOURCE_PREFIX +
        MessageDigest.getInstance("SHA-256").digest(jobId.toByteArray(Charsets.UTF_8)).joinToString("") { "%02x".format(it) }

    private fun findReservation(marker: String): Row? = resolver.query(
        downloads, PROJECTION,
        "${MediaStore.MediaColumns.OWNER_PACKAGE_NAME} = ? AND ${MediaStore.Downloads.DOWNLOAD_URI} = ?",
        arrayOf(context.packageName, marker), "${MediaStore.MediaColumns._ID} ASC",
    )?.use { cursor ->
        if (!cursor.moveToFirst()) null else row(cursor, ContentUris.withAppendedId(downloads, cursor.getLong(0)))
    }

    private fun reservationUri(locator: String): Uri {
        val uri = Uri.parse(locator)
        require(uri.scheme == "content" && uri.authority == "media" && uri.pathSegments.size == 3 &&
            uri.pathSegments[0] == MediaStore.VOLUME_EXTERNAL_PRIMARY && uri.pathSegments[1] == "downloads" &&
            uri.pathSegments[2].toLongOrNull()?.let { it > 0 } == true) { "The output reservation is invalid." }
        return uri
    }

    private fun readRow(uri: Uri): Row? = resolver.query(uri, PROJECTION, null, null, null)?.use {
        if (it.moveToFirst()) row(it, uri) else null
    }

    private fun row(cursor: android.database.Cursor, uri: Uri) = Row(
        uri = uri,
        name = cursor.getString(1),
        mime = cursor.getString(2),
        pending = cursor.getInt(3) != 0,
        owner = cursor.getString(4),
        marker = cursor.getString(5),
        recordedSize = if (cursor.isNull(6)) null else cursor.getLong(6),
    )

    private fun requireOwned(row: Row) {
        require(row.owner == context.packageName && row.marker?.startsWith(RESOURCE_PREFIX) == true) {
            "The destination is not an output reservation owned by Velmux."
        }
    }

    private fun actualSize(uri: Uri): Long = resolver.openFileDescriptor(uri, "r")?.use { descriptor ->
        descriptor.statSize.takeIf { it >= 0 } ?: throw PlatformFailure("Shared storage did not expose the saved file size.")
    } ?: throw PlatformFailure("The saved file is no longer available.")

    private fun published(row: Row): NativePublished {
        requireOwned(row)
        val size = actualSize(row.uri)
        if (size <= 0 || row.recordedSize?.let { it > 0 && it != size } == true) {
            throw PlatformFailure("The published output is incomplete or has changed.")
        }
        return NativePublished(row.uri.toString(), row.name, size)
    }

    private inline fun <T> platformOperation(message: String, action: () -> T): T = try {
        action()
    } catch (error: PlatformFailure) {
        throw error
    } catch (error: IllegalArgumentException) {
        throw PlatformFailure(error.message?.take(240) ?: message)
    } catch (_: Exception) {
        throw PlatformFailure(message)
    }

    private data class Row(
        val uri: Uri, val name: String, val mime: String, val pending: Boolean,
        val owner: String?, val marker: String?, val recordedSize: Long?,
    ) {
        fun reservation() = NativeReservation(uri.toString(), name, mime)
    }

    private companion object {
        val publicationLock = Any()
        const val RESOURCE_PREFIX = "velmux-generated://jobs/"
        val PROJECTION = arrayOf(
            MediaStore.MediaColumns._ID, MediaStore.MediaColumns.DISPLAY_NAME,
            MediaStore.MediaColumns.MIME_TYPE, MediaStore.MediaColumns.IS_PENDING,
            MediaStore.MediaColumns.OWNER_PACKAGE_NAME, MediaStore.Downloads.DOWNLOAD_URI,
            MediaStore.MediaColumns.SIZE,
        )
    }
}

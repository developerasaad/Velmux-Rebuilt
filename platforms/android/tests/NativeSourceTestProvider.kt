package com.velmux.mobile.testing

import android.content.ContentProvider
import android.content.ContentValues
import android.content.Intent
import android.content.res.AssetFileDescriptor
import android.database.Cursor
import android.database.MatrixCursor
import android.net.Uri
import android.os.Binder
import android.os.Bundle
import android.os.ParcelFileDescriptor
import android.provider.OpenableColumns
import java.io.File

/** A separate test-APK provider exposes only manufactured bytes, never user media. */
class NativeSourceTestProvider : ContentProvider() {
    override fun onCreate() = true
    override fun getType(uri: Uri) = "application/octet-stream"

    override fun call(method: String, arg: String?, extras: Bundle?): Bundle {
        val root = requireNotNull(context)
        val key = requireNotNull(arg).also { require(it.matches(Regex("[a-f0-9-]{36}"))) }
        val file = File(root.cacheDir, "source-fixture-$key.bin")
        val uri = Uri.parse("content://com.velmux.mobile.test.sources/$key")
        val identity = Binder.clearCallingIdentity()
        try {
            when (method) {
                "create" -> {
                    file.writeBytes(ByteArray(17) { 0x7f } + ByteArray(64) { it.toByte() } + ByteArray(11) { 0x7e })
                    root.grantUriPermission("com.velmux.mobile", uri,
                        Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION)
                }
                "remove" -> {
                    root.revokeUriPermission("com.velmux.mobile", uri, Intent.FLAG_GRANT_READ_URI_PERMISSION)
                    file.delete()
                }
                else -> error("Unknown fixture request")
            }
        } finally {
            Binder.restoreCallingIdentity(identity)
        }
        return Bundle().apply { putString("uri", uri.toString()) }
    }

    override fun query(uri: Uri, projection: Array<out String>?, selection: String?, selectionArgs: Array<out String>?, sortOrder: String?): Cursor {
        val columns = projection ?: arrayOf(OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE)
        return MatrixCursor(columns).apply {
            addRow(columns.map {
                when (it) {
                    OpenableColumns.DISPLAY_NAME -> "provider-slice.bin"
                    OpenableColumns.SIZE -> 64L
                    else -> null
                }
            })
        }
    }

    override fun openAssetFile(uri: Uri, mode: String): AssetFileDescriptor {
        require(mode == "r")
        val key = requireNotNull(uri.lastPathSegment).also { require(it.matches(Regex("[a-f0-9-]{36}"))) }
        val file = File(requireNotNull(context).cacheDir, "source-fixture-$key.bin")
        return AssetFileDescriptor(ParcelFileDescriptor.open(file, ParcelFileDescriptor.MODE_READ_ONLY), 17, 64)
    }

    override fun insert(uri: Uri, values: ContentValues?): Uri? = throw UnsupportedOperationException()
    override fun delete(uri: Uri, selection: String?, selectionArgs: Array<out String>?): Int = throw UnsupportedOperationException()
    override fun update(uri: Uri, values: ContentValues?, selection: String?, selectionArgs: Array<out String>?): Int = throw UnsupportedOperationException()
}

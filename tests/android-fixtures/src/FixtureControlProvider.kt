package com.velmux.mobile.testing

import android.content.ContentProvider
import android.content.ContentValues
import android.content.Intent
import android.database.Cursor
import android.net.Uri
import android.os.Binder
import android.os.Bundle
import android.provider.DocumentsContract

/** No general document access: this control endpoint only grants manufactured fixtures. */
class FixtureControlProvider : ContentProvider() {
    override fun onCreate() = true

    override fun call(method: String, arg: String?, extras: Bundle?): Bundle {
        val root = requireNotNull(context)
        FixtureFiles.requireControlCaller(root)
        if (method == "write-evidence") return retainEvidence(root, requireNotNull(arg), requireNotNull(extras))
        require(method == "grant-fixture" || method == "revoke-fixture") { "Unknown fixture control request" }
        val name = requireNotNull(arg)
        FixtureFiles.open(root, name)
        val uri = DocumentsContract.buildDocumentUri("com.velmux.mobile.test.documents", name)
        val identity = Binder.clearCallingIdentity()
        try {
            if (method == "grant-fixture") {
                root.grantUriPermission("com.velmux.mobile", uri,
                    Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION)
            } else root.revokeUriPermission("com.velmux.mobile", uri, Intent.FLAG_GRANT_READ_URI_PERMISSION)
        } finally { Binder.restoreCallingIdentity(identity) }
        return Bundle().apply { putString("uri", uri.toString()) }
    }

    private fun retainEvidence(context: android.content.Context, name: String, extras: Bundle): Bundle {
        require(name.matches(Regex("[A-Za-z0-9_.-]{1,96}")) &&
            name.substringAfterLast('.') in setOf("png", "xml", "txt", "mp4", "m4a", "flac")) { "Invalid verification artifact name" }
        val bytes = requireNotNull(extras.getByteArray("bytes"))
        val offset = extras.getLong("offset", -1)
        require(bytes.size <= 256 * 1024 && offset in 0..4L * 1024 * 1024 && offset + bytes.size <= 4L * 1024 * 1024)
        val root = java.io.File(context.filesDir, "evidence")
        check(root.isDirectory || root.mkdirs())
        synchronized(evidenceLock) {
            val output = java.io.File(root, name)
            require(offset == 0L || (output.isFile && output.length() == offset)) { "Verification artifact chunks arrived out of order" }
            java.io.FileOutputStream(output, offset != 0L).use { stream ->
                stream.write(bytes)
                stream.fd.sync()
            }
        }
        return Bundle().apply { putLong("size", offset + bytes.size) }
    }

    private companion object { val evidenceLock = Any() }

    override fun getType(uri: Uri): String? = null
    override fun query(uri: Uri, projection: Array<out String>?, selection: String?, selectionArgs: Array<out String>?, sortOrder: String?): Cursor? = throw UnsupportedOperationException()
    override fun insert(uri: Uri, values: ContentValues?): Uri? = throw UnsupportedOperationException()
    override fun delete(uri: Uri, selection: String?, selectionArgs: Array<out String>?): Int = throw UnsupportedOperationException()
    override fun update(uri: Uri, values: ContentValues?, selection: String?, selectionArgs: Array<out String>?): Int = throw UnsupportedOperationException()
}

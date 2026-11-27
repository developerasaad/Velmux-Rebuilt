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

    override fun getType(uri: Uri): String? = null
    override fun query(uri: Uri, projection: Array<out String>?, selection: String?, selectionArgs: Array<out String>?, sortOrder: String?): Cursor? = throw UnsupportedOperationException()
    override fun insert(uri: Uri, values: ContentValues?): Uri? = throw UnsupportedOperationException()
    override fun delete(uri: Uri, selection: String?, selectionArgs: Array<out String>?): Int = throw UnsupportedOperationException()
    override fun update(uri: Uri, values: ContentValues?, selection: String?, selectionArgs: Array<out String>?): Int = throw UnsupportedOperationException()
}

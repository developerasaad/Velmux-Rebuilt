package com.velmux.mobile.testing

import android.database.Cursor
import android.database.MatrixCursor
import android.os.CancellationSignal
import android.os.ParcelFileDescriptor
import android.provider.DocumentsContract
import android.provider.DocumentsProvider
import java.io.File
import java.io.FileNotFoundException

/** Read-only sources exist only in the standalone verification APK, never in the product. */
class FixtureDocumentsProvider : DocumentsProvider() {
    override fun onCreate() = true

    override fun queryRoots(projection: Array<out String>?): Cursor = MatrixCursor(projection ?: ROOT_COLUMNS).apply {
        val values = mapOf<String, Any?>(
            DocumentsContract.Root.COLUMN_ROOT_ID to ROOT,
            DocumentsContract.Root.COLUMN_DOCUMENT_ID to ROOT,
            DocumentsContract.Root.COLUMN_TITLE to "Velmux Test Sources",
            DocumentsContract.Root.COLUMN_SUMMARY to "Read-only verification media",
            DocumentsContract.Root.COLUMN_MIME_TYPES to "video/*\naudio/*\napplication/octet-stream",
            DocumentsContract.Root.COLUMN_FLAGS to DocumentsContract.Root.FLAG_LOCAL_ONLY,
            DocumentsContract.Root.COLUMN_ICON to android.R.drawable.ic_menu_gallery,
        )
        addRow(columnNames.map { values[it] })
    }

    override fun queryDocument(documentId: String, projection: Array<out String>?): Cursor =
        MatrixCursor(projection ?: DOCUMENT_COLUMNS).apply { addDocument(documentId) }

    override fun queryChildDocuments(parentDocumentId: String, projection: Array<out String>?, sortOrder: String?): Cursor {
        if (parentDocumentId != ROOT) throw FileNotFoundException("Unknown fixture directory")
        return MatrixCursor(projection ?: DOCUMENT_COLUMNS).apply { FIXTURES.forEach { addDocument(it) } }
    }

    override fun isChildDocument(parentDocumentId: String, documentId: String): Boolean =
        parentDocumentId == ROOT && documentId in FIXTURES

    override fun openDocument(documentId: String, mode: String, signal: CancellationSignal?): ParcelFileDescriptor {
        signal?.throwIfCanceled()
        if (mode != "r") throw FileNotFoundException("Verification sources are read-only")
        return ParcelFileDescriptor.open(fixture(documentId), ParcelFileDescriptor.MODE_READ_ONLY)
    }

    private fun MatrixCursor.addDocument(id: String) {
        val directory = id == ROOT
        val file = if (directory) null else fixture(id)
        val values = mapOf<String, Any?>(
            DocumentsContract.Document.COLUMN_DOCUMENT_ID to id,
            DocumentsContract.Document.COLUMN_DISPLAY_NAME to if (directory) "Velmux Test Sources" else id,
            DocumentsContract.Document.COLUMN_MIME_TYPE to if (directory) DocumentsContract.Document.MIME_TYPE_DIR else mime(id),
            DocumentsContract.Document.COLUMN_FLAGS to 0,
            DocumentsContract.Document.COLUMN_SIZE to file?.length(),
            DocumentsContract.Document.COLUMN_LAST_MODIFIED to file?.lastModified(),
        )
        addRow(columnNames.map { values[it] })
    }

    private fun fixture(id: String): File = FixtureFiles.open(requireNotNull(context), id)

    private fun mime(name: String) = when (name.substringAfterLast('.')) {
        "mp4" -> "video/mp4"
        "mkv" -> "video/x-matroska"
        "wav" -> "audio/wav"
        "flac" -> "audio/flac"
        else -> "application/octet-stream"
    }

    private companion object {
        const val AUTHORITY = "com.velmux.mobile.test.documents"
        const val ROOT = "verification-media"
        val FIXTURES = FixtureFiles.names
        val ROOT_COLUMNS = arrayOf(DocumentsContract.Root.COLUMN_ROOT_ID, DocumentsContract.Root.COLUMN_DOCUMENT_ID,
            DocumentsContract.Root.COLUMN_TITLE, DocumentsContract.Root.COLUMN_SUMMARY, DocumentsContract.Root.COLUMN_MIME_TYPES,
            DocumentsContract.Root.COLUMN_FLAGS, DocumentsContract.Root.COLUMN_ICON)
        val DOCUMENT_COLUMNS = arrayOf(DocumentsContract.Document.COLUMN_DOCUMENT_ID, DocumentsContract.Document.COLUMN_DISPLAY_NAME,
            DocumentsContract.Document.COLUMN_MIME_TYPE, DocumentsContract.Document.COLUMN_FLAGS,
            DocumentsContract.Document.COLUMN_SIZE, DocumentsContract.Document.COLUMN_LAST_MODIFIED)
    }
}

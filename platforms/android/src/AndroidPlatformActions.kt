package com.velmux.mobile

import android.app.Activity
import android.content.ClipData
import android.content.Intent
import android.net.Uri
import android.provider.OpenableColumns
import androidx.activity.ComponentActivity
import androidx.activity.result.contract.ActivityResultContracts
import androidx.lifecycle.DefaultLifecycleObserver
import androidx.lifecycle.LifecycleOwner
import com.velmux.ui.app.PlatformActions
import com.velmux.ui.app.SelectedSource

/** Construct during Activity.onCreate, before the activity reaches STARTED. */
class AndroidPlatformActions(private val activity: ComponentActivity) : PlatformActions {
    private var pendingSelection: ((SelectedSource?, String?) -> Unit)? = null
    private var selectionToken: Long? = null
    private val picker = activity.registerForActivityResult(ActivityResultContracts.StartActivityForResult()) { result ->
        val completion = pendingSelection ?: return@registerForActivityResult
        pendingSelection = null
        if (result.resultCode != Activity.RESULT_OK) {
            finishSelection(null)
            completion(null, null)
        } else {
            val intent = result.data
            val uri = intent?.data
            if (intent == null || uri == null || intent.clipData?.itemCount?.let { it > 1 } == true) {
                finishSelection(null)
                completion(null, "Select one readable media document.")
            } else {
                var selected: SelectedSource? = null
                var failure: String? = null
                try {
                    if (intent.flags and Intent.FLAG_GRANT_READ_URI_PERMISSION == 0) {
                        throw SecurityException("The provider did not grant read access.")
                    }
                    // The delivered transient grant supports metadata access.
                    // Rust's first retained reference acquires lasting access.
                    val name = activity.contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use {
                        if (it.moveToFirst()) it.getString(0)?.take(512) else null
                    } ?: "Selected media"
                    selected = SelectedSource(uri.toString(), name)
                } catch (_: SecurityException) {
                    failure = "This document provider did not grant readable access. Select the file again using the system document picker."
                } catch (_: Exception) {
                    failure = "The selected document could not be read. Select it again."
                }
                finishSelection(selected?.locator)
                completion(selected, failure)
            }
        }
    }

    init {
        activity.lifecycle.addObserver(object : DefaultLifecycleObserver {
            override fun onDestroy(owner: LifecycleOwner) {
                val completion = pendingSelection
                pendingSelection = null
                finishSelection(null)
                completion?.invoke(null, null)
            }
        })
    }

    override fun selectSource(video: Boolean, completion: (SelectedSource?, String?) -> Unit) {
        if (pendingSelection != null) {
            completion(null, "A document selection is already open.")
            return
        }
        pendingSelection = completion
        try {
            selectionToken = AndroidDocumentGrants.beginSelection(activity)
            picker.launch(selectionIntent(video))
        } catch (error: Exception) {
            if (pendingSelection !== completion) throw error
            pendingSelection = null
            finishSelection(null)
            completion(null, "The system document picker is unavailable.")
        }
    }

    private fun finishSelection(locator: String?) {
        val token = selectionToken ?: return
        selectionToken = null
        AndroidDocumentGrants.finishSelection(activity, token, locator)
    }

    override fun openOutput(locator: String, completion: (String?) -> Unit) {
        launchOutput(locator, share = false, completion = completion)
    }

    override fun shareOutput(locator: String, completion: (String?) -> Unit) {
        launchOutput(locator, share = true, completion = completion)
    }

    private fun launchOutput(locator: String, share: Boolean, completion: (String?) -> Unit) {
        try {
            val uri = Uri.parse(locator)
            require(uri.scheme == "content")
            activity.contentResolver.openFileDescriptor(uri, "r")?.use { } ?: throw IllegalStateException("The saved result is no longer accessible.")
            val type = activity.contentResolver.getType(uri) ?: "application/octet-stream"
            val intent = Intent(if (share) Intent.ACTION_SEND else Intent.ACTION_VIEW).apply {
                addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
                clipData = ClipData.newRawUri("Velmux output", uri)
                if (share) {
                    this.type = type
                    putExtra(Intent.EXTRA_STREAM, uri)
                } else {
                    setDataAndType(uri, type)
                }
            }
            activity.startActivity(Intent.createChooser(intent, if (share) "Share result" else "Open result"))
        } catch (_: Exception) {
            completion("The saved result is unavailable or no installed application can open it.")
            return
        }
        completion(null)
    }

    companion object {
        fun selectionIntent(video: Boolean) = Intent(Intent.ACTION_OPEN_DOCUMENT).apply {
            addCategory(Intent.CATEGORY_OPENABLE)
            type = if (video) "video/*" else "audio/*"
            putExtra(Intent.EXTRA_ALLOW_MULTIPLE, false)
            addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION)
        }
    }
}
